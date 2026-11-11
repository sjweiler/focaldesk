use crate::{
    attachments::{Attachment, DraftAttachment, MAX_BYTES},
    store::Store,
    Message,
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";
#[derive(Deserialize)]
pub struct ClientFile {
    pub installed: ClientConfig,
}
#[derive(Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}
#[derive(Serialize, Deserialize)]
struct Credential {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}
#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn entry(email: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("org.focaldesk.FocalMail.gmail", email)?)
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::service::OperationCancelled.into());
    }
    Ok(())
}
fn json(response: reqwest::blocking::Response) -> Result<Value> {
    // Do not display token responses, authorization URLs, or provider bodies in errors.
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(crate::service::ReconnectRequired.into());
    }
    if !status.is_success() {
        let body: Value = response.json().unwrap_or(Value::Null);
        bail!(
            "Google request failed ({status}). {}",
            error_guidance(&body)
        );
    }
    Ok(response.json()?)
}
fn error_guidance(body: &Value) -> &'static str {
    // Only interpret known reason codes; never expose provider bodies or token data.
    let reasons = body["error"]["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(body["error"]["details"].as_array().into_iter().flatten())
        .filter_map(|v| v["reason"].as_str())
        .collect::<Vec<_>>();
    for reason in reasons {
        match reason {
            "insufficientPermissions" | "ACCESS_TOKEN_SCOPE_INSUFFICIENT" => return "Gmail permission is missing. Reconnect in Settings and approve the Gmail read/modify/send permission (gmail.modify).",
            "accessNotConfigured" | "SERVICE_DISABLED" => return "Enable Gmail API in the same Google Cloud project as the Desktop OAuth client JSON, wait a few minutes, then Refresh.",
            "rateLimitExceeded" | "userRateLimitExceeded" | "RATE_LIMIT_EXCEEDED" => return "Gmail request-rate limit reached. Sync retries temporarily with backoff; if exhausted, wait a minute before Refresh. Check Gmail API per-minute quotas if it persists.",
            "quotaExceeded" | "dailyLimitExceeded" => return "Gmail project quota exhausted. Check Gmail API quotas in the OAuth client's Google Cloud project; wait for the quota reset or adjust the configured limit. Reconnecting will not reset quota.",
            "domainPolicy" => return "Your Google Workspace administrator has blocked this app's Gmail access. Ask the administrator to allow the OAuth client.",
            _ => {}
        }
    }
    "Google did not return a recognized reason. Check Gmail API access and granted permissions, then reconnect if necessary."
}

fn pause(cancel: &AtomicBool, duration: Duration) -> Result<()> {
    let end = Instant::now() + duration;
    loop {
        cancelled(cancel)?;
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

fn transient_quota(body: &Value) -> bool {
    body["error"]["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(body["error"]["details"].as_array().into_iter().flatten())
        .any(|v| {
            matches!(
                v["reason"].as_str(),
                Some("rateLimitExceeded" | "userRateLimitExceeded" | "RATE_LIMIT_EXCEEDED")
            )
        })
}

// A full message costs 60 quota units under Google's May 2026 quotas.
// At most 75 reads/minute leaves headroom beneath 6,000 units/user/minute.
fn read_request(
    request: reqwest::blocking::RequestBuilder,
    cancel: &AtomicBool,
) -> Result<reqwest::blocking::Response> {
    read_with_wait(request, cancel, |duration| pause(cancel, duration))
}

fn read_with_wait(
    request: reqwest::blocking::RequestBuilder,
    cancel: &AtomicBool,
    wait: impl FnMut(Duration) -> Result<()>,
) -> Result<reqwest::blocking::Response> {
    read_with_pacing(request, cancel, Duration::from_millis(800), wait)
}
fn read_with_pacing(
    request: reqwest::blocking::RequestBuilder,
    cancel: &AtomicBool,
    pace: Duration,
    mut wait: impl FnMut(Duration) -> Result<()>,
) -> Result<reqwest::blocking::Response> {
    for attempt in 0..=6 {
        cancelled(cancel)?;
        wait(pace)?;
        let response = request
            .try_clone()
            .context("Cannot clone Gmail read request")?
            .send()?;
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(response);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(crate::service::ReconnectRequired.into());
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.parse::<u64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(v).ok().map(|date| {
                        (date.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64
                    })
                })
            });
        let body: Value = response.json().unwrap_or(Value::Null);
        let retry = status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
            || (status == reqwest::StatusCode::FORBIDDEN && transient_quota(&body));
        if !retry || attempt == 6 || retry_after.is_some_and(|s| s > 60) {
            bail!(
                "Google request failed ({status}). {}",
                error_guidance(&body)
            );
        }
        let delay = Duration::from_secs((1u64 << attempt).max(retry_after.unwrap_or_default()))
            + Duration::from_millis(u64::from(rand::random::<u16>()) % 1000);
        wait(delay)?;
    }
    unreachable!()
}

fn get(client: &Client, token: &str, path: &str) -> Result<Value> {
    json(
        client
            .get(format!("{API}/{path}"))
            .bearer_auth(token)
            .send()?,
    )
}

pub fn connect(path: &str, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
    let file: ClientFile = serde_json::from_slice(
        &std::fs::read(path.trim()).context("Cannot read Google desktop OAuth JSON")?,
    )
    .context("Expected Google's downloaded Desktop app OAuth JSON (installed.client_id)")?;
    connect_config(file.installed, None, cancel, progress)
}

pub fn reconnect(email: &str, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
    let saved: Credential = serde_json::from_str(&entry(email)?.get_password()?)?;
    connect_config(
        ClientConfig {
            client_id: saved.client_id,
            client_secret: saved.client_secret,
        },
        Some(email),
        cancel,
        progress,
    )
}

fn connect_config(
    config: ClientConfig,
    expected: Option<&str>,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(&str),
) -> Result<String> {
    if config.client_id.trim().is_empty() {
        bail!("OAuth client ID is empty");
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let verifier = random();
    let state = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", &redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ]);
    webbrowser::open(url.as_str()).context("Could not open system browser")?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let code = loop {
        cancelled(&cancel)?;
        if Instant::now() > deadline {
            bail!("Sign-in timed out. Try Connect Gmail again.");
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                let mut line = String::new();
                let read = BufReader::new((&stream).take(8192)).read_line(&mut line);
                if read.is_err() {
                    continue;
                }
                let result = callback(&line, &state);
                let text = if result.is_ok() {
                    "Google callback received. FocalMail must still exchange the code and save credentials. Return to FocalMail for the result."
                } else {
                    "Invalid authorization response. Return to FocalMail and try again."
                };
                let _ = write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",text.len(),text);
                match result {
                    Ok(Some(code)) => break code,
                    Ok(None) => bail!("Google authorization declined"),
                    Err(_) => continue,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(e) => return Err(e.into()),
        }
    };
    cancelled(&cancel)?;
    progress("Google callback received · exchanging authorization code…");
    let client = client()?;
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
        ])
        .send()?;
    let token: Token = serde_json::from_value(json(response)?)?;
    progress("Google token received · checking Gmail API access…");
    let profile = get(&client, &token.access_token, "profile").context("Could not access Gmail profile; enable Gmail API in the OAuth client project and reconnect")?;
    let email = profile["emailAddress"]
        .as_str()
        .context("Missing Gmail account address")?
        .to_string();
    anyhow::ensure!(
        expected.is_none_or(|expected| expected.eq_ignore_ascii_case(&email)),
        "Signed into a different account; reconnect the selected account"
    );
    crate::providers::ensure_provider(&crate::settings::Profile {
        email: email.clone(),
        ..Default::default()
    })?;
    let credential = Credential {
        client_id: config.client_id,
        client_secret: config.client_secret,
        refresh_token: token
            .refresh_token
            .context("No refresh token returned; reconnect and grant offline access")?,
    };
    cancelled(&cancel)?;
    progress("Gmail account verified · saving credentials in the desktop keyring…");
    entry(&email)?
        .set_password(&serde_json::to_string(&credential)?)
        .map_err(|e| anyhow::anyhow!("Could not save Gmail credentials in Secret Service: {e}. Unlock or start your desktop keyring and retry."))?;
    // Account is recorded only after secure storage succeeds; cache sync may be retried.
    let mut store = Store::open()?;
    if !store.accounts()?.contains(&email) {
        store.snapshot(&email, &[], "")?;
    }
    progress(&format!(
        "Account added: {email} · syncing mailbox; paced sync reads about 75 messages/minute; large mailboxes take time…"
    ));
    let sync_result = sync_token(&client, &token.access_token, &email, &cancel);
    Store::open()?.record_status(&email, &sync_result)?;
    match sync_result {
        Ok(()) => Ok(format!("Connected {email} · mailbox synchronized")),
        Err(e) => Ok(format!(
            "Account added: {email}; initial sync did not finish: {e}. Use Refresh to retry."
        )),
    }
}

use std::io::Read;
fn callback(line: &str, expected: &str) -> Result<Option<String>> {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("GET") {
        bail!("Invalid callback method");
    }
    let target = parts.next().context("Missing callback target")?;
    if !target.starts_with("/?") {
        bail!("Invalid callback path");
    }
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
    let values: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = values.iter().filter(|(k, _)| k == "state").collect();
    if states.len() != 1 || states[0].1 != expected {
        bail!("OAuth state mismatch");
    }
    if values.iter().any(|(k, _)| k == "error") {
        return Ok(None);
    }
    let codes: Vec<_> = values.iter().filter(|(k, _)| k == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() {
        bail!("Missing authorization code");
    }
    Ok(Some(codes[0].1.to_string()))
}

pub fn sync_account(email: &str, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    let client = client()?;
    let token = access_token(&client, email)?;
    sync_token(&client, &token, email, cancel)
}
fn access_token(client: &Client, email: &str) -> Result<String> {
    let credential: Credential = serde_json::from_str(
        &entry(email)?
            .get_password()
            .map_err(crate::service::credential_error)?,
    )?;
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", credential.client_id.as_str()),
            ("client_secret", credential.client_secret.as_str()),
            ("refresh_token", credential.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    let token = refresh_response(status, body)?;
    if let Some(refresh) = token.refresh_token {
        entry(email)?.set_password(&serde_json::to_string(&Credential {
            refresh_token: refresh,
            ..credential
        })?)?;
    }
    Ok(token.access_token)
}

fn refresh_response(status: reqwest::StatusCode, body: Value) -> Result<Token> {
    if body["error"] == "invalid_grant" || status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(crate::service::ReconnectRequired.into());
    }
    anyhow::ensure!(
        status.is_success(),
        "Google token refresh failed ({status}); check network and application configuration"
    );
    Ok(serde_json::from_value(body)?)
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Action {
    Read(bool),
    Star(bool),
    Archive,
    Trash,
}

pub struct MutationSession {
    client: Client,
    token: String,
}
impl MutationSession {
    pub fn open(email: &str) -> Result<Self> {
        let client = client()?;
        let token = access_token(&client, email)?;
        Ok(Self { client, token })
    }
    pub fn apply(&mut self, message: &Message, action: Action) -> Result<()> {
        mutate(&self.client, &self.token, &message.id, action, API)
    }
}
pub fn forget_credentials(email: &str) -> Result<()> {
    match entry(email)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn modify(message: &Message, action: Action, cancel: Arc<AtomicBool>) -> Result<String> {
    cancelled(&cancel)?;
    let client = client()?;
    let token = access_token(&client, &message.account)?;
    mutate(&client, &token, &message.id, action, API)?;
    match sync_token(&client, &token, &message.account, &cancel) {
        Ok(()) => Ok("Mail updated".into()),
        Err(e) => Ok(format!("Mail updated on Gmail; refresh needed: {e}")),
    }
}

fn mutate(client: &Client, token: &str, id: &str, action: Action, api: &str) -> Result<()> {
    let (add, remove): (Vec<&str>, Vec<&str>) = match action {
        Action::Read(true) => (vec![], vec!["UNREAD"]),
        Action::Read(false) => (vec!["UNREAD"], vec![]),
        Action::Star(true) => (vec!["STARRED"], vec![]),
        Action::Star(false) => (vec![], vec!["STARRED"]),
        Action::Archive => (vec![], vec!["INBOX"]),
        Action::Trash => {
            json(
                client
                    .post(format!("{api}/messages/{id}/trash"))
                    .bearer_auth(token)
                    .send()?,
            )?;
            return Ok(());
        }
    };
    json(
        client
            .post(format!("{api}/messages/{id}/modify"))
            .bearer_auth(token)
            .json(&serde_json::json!({"addLabelIds": add, "removeLabelIds": remove}))
            .send()?,
    )?;
    Ok(())
}

#[cfg(test)]
fn raw_message(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    attachments: &[DraftAttachment],
) -> Result<String> {
    raw_message_html(from, to, subject, body, None, attachments)
}

fn raw_message_html(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    html: Option<&str>,
    attachments: &[DraftAttachment],
) -> Result<String> {
    for value in [from, to, subject] {
        anyhow::ensure!(
            !value.contains(['\r', '\n']),
            "Mail headers cannot contain newlines"
        );
    }
    for value in [from, to] {
        let addresses = mailparse::addrparse(value).context("Invalid email address")?;
        anyhow::ensure!(addresses.count_addrs() > 0, "Enter a recipient and sender");
    }
    use base64::engine::general_purpose::STANDARD;
    let subject = STANDARD.encode(subject.as_bytes());
    let encoded = STANDARD.encode(body.replace("\r\n", "\n").replace('\n', "\r\n").as_bytes());
    let wrapped = encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\r\n");
    anyhow::ensure!(
        body.len() <= 1024 * 1024,
        "Message text may be at most 1 MiB"
    );
    anyhow::ensure!(
        attachments.iter().map(|f| f.data.len()).sum::<usize>() <= MAX_BYTES,
        "Attachments may total at most 20 MiB"
    );
    let headers = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: =?UTF-8?B?{subject}?=\r\nMIME-Version: 1.0\r\n"
    );
    let plain = format!("Content-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{wrapped}\r\n");
    let content = if let Some(html) = html {
        anyhow::ensure!(
            html.len() <= 1024 * 1024,
            "Message HTML may be at most 1 MiB"
        );
        let html = crate::compose::sanitize(html);
        let encoded = STANDARD.encode(html.as_bytes());
        let encoded = encoded
            .as_bytes()
            .chunks(76)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        let boundary = format!("focalmail-alt-{}", random());
        format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{plain}--{boundary}\r\nContent-Type: text/html; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n--{boundary}--\r\n")
    } else {
        plain
    };
    if attachments.is_empty() {
        return Ok(URL_SAFE_NO_PAD.encode(format!("{headers}{content}")));
    }
    let boundary = format!("focalmail-{}", random());
    let mut mime = format!("{headers}Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{content}");
    for attachment in attachments {
        // RFC 2231 percent encoding prevents filename/header injection and preserves Unicode.
        let name = crate::attachments::filename(&attachment.name)
            .bytes()
            .map(|b| format!("%{b:02X}"))
            .collect::<String>();
        let name = name
            .as_bytes()
            .chunks(45)
            .enumerate()
            .map(|(index, chunk)| {
                format!(
                    ";\r\n filename*{index}*={}{}",
                    if index == 0 { "UTF-8''" } else { "" },
                    std::str::from_utf8(chunk).unwrap()
                )
            })
            .collect::<String>();
        let encoded = STANDARD.encode(&attachment.data);
        let data = encoded
            .as_bytes()
            .chunks(76)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        mime.push_str(&format!("--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment{name}\r\nContent-Transfer-Encoding: base64\r\n\r\n{data}\r\n"));
    }
    mime.push_str(&format!("--{boundary}--\r\n"));
    Ok(URL_SAFE_NO_PAD.encode(mime))
}

fn reference_ids(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|s| {
            s.starts_with('<')
                && s.ends_with('>')
                && s.len() <= 250
                && s.len() > 2
                && !s.chars().any(char::is_control)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\r\n ")
}
pub(crate) fn draft_message(
    draft: &crate::drafts::Draft,
    files: &[DraftAttachment],
) -> Result<Value> {
    anyhow::ensure!(
        !draft.cc.contains(['\r', '\n']),
        "Cc cannot contain newlines"
    );
    if !draft.cc.trim().is_empty() {
        anyhow::ensure!(
            !crate::drafts::addresses(&draft.cc).is_empty(),
            "Enter valid Cc addresses"
        );
    }
    let raw = raw_message_html(
        &draft.from,
        &draft.to,
        &draft.subject,
        &draft.body,
        draft.html_body.as_deref(),
        files,
    )?;
    let mut headers = String::new();
    if !draft.cc.is_empty() {
        headers.push_str(&format!("Cc: {}\r\n", draft.cc));
    }
    let threaded = draft.from.eq_ignore_ascii_case(&draft.reply_account)
        && draft.subject == draft.reply_subject;
    let reply = reference_ids(&draft.in_reply_to);
    if threaded && !reply.is_empty() {
        headers.push_str(&format!(
            "In-Reply-To: {reply}\r\nReferences: {}\r\n",
            reference_ids(&draft.references)
        ));
    }
    let mut bytes = headers.into_bytes();
    bytes.extend(URL_SAFE_NO_PAD.decode(raw)?);
    let mut payload = serde_json::json!({"raw":URL_SAFE_NO_PAD.encode(bytes)});
    if threaded && !reply.is_empty() && !draft.thread_id.is_empty() {
        payload["threadId"] = draft.thread_id.clone().into();
    }
    Ok(payload)
}

pub fn send(
    draft: &crate::drafts::Draft,
    files: &[DraftAttachment],
    cancel: Arc<AtomicBool>,
) -> Result<String> {
    let payload = draft_message(draft, files)?;
    let client = client()?;
    let token = access_token(&client, &draft.from)?;
    cancelled(&cancel)?;
    deliver_payload(&client, &token, &payload, API)?;
    if let Err(e) = Store::open().and_then(|s| s.delete_local_draft(&draft.id)) {
        return Ok(format!(
            "Message sent; could not clear this saved draft: {e}. Do not resend."
        ));
    }
    Ok("Message sent · background sync will update Sent".into())
}

#[cfg(test)]
fn deliver(client: &Client, token: &str, raw: &str, api: &str) -> Result<()> {
    deliver_payload(client, token, &serde_json::json!({"raw": raw}), api)
}

fn deliver_payload(client: &Client, token: &str, payload: &Value, api: &str) -> Result<()> {
    // Never retry a send: a lost response may mean delivery succeeded.
    let response = client
        .post(format!("{api}/messages/send"))
        .bearer_auth(token)
        .json(payload)
        .send()
        .context("Send outcome unknown; check Sent before retrying")?;
    let status = response.status();
    if !status.is_success() {
        bail!("Gmail send returned {status}; check Sent before retrying. Reconnect if access was denied.");
    }
    // A successful status confirms delivery even if the response body is unreadable.
    Ok(())
}

fn sync_token(client: &Client, token: &str, email: &str, cancel: &AtomicBool) -> Result<()> {
    sync_with_store(client, token, email, cancel, &mut Store::open()?, API)
}

// Keep arrivals visible while an initial import works through historical pages.
// Only missing IDs are downloaded; this never replaces the scan's saved cursor.
fn refresh_recent_inbox(
    client: &Client,
    token: &str,
    email: &str,
    cancel: &AtomicBool,
    store: &Store,
    api: &str,
    downloaded: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    let listed = json(read_request(
        client
            .get(format!("{api}/messages"))
            .bearer_auth(token)
            .query(&[
                ("maxResults", "50"),
                ("labelIds", "INBOX"),
                ("q", "newer_than:7d"),
            ]),
        cancel,
    )?)?;
    for item in listed["messages"].as_array().into_iter().flatten().take(50) {
        cancelled(cancel)?;
        let id = item["id"]
            .as_str()
            .context("Recent Gmail message missing ID")?;
        if downloaded.contains(id) {
            continue;
        }
        let Some(message) = retrieve_sync_message(client, token, email, id, cancel, store, api)?
        else {
            continue;
        };
        store.stage_message(email, &message)?;
        downloaded.insert(id.to_string());
        store.sync_progress(
            email,
            if store.has_committed_messages(email)? {
                "checking_mail"
            } else {
                "importing"
            },
            downloaded.len(),
            None,
        )?;
    }
    Ok(())
}

// Gmail message bodies are cached on their first retrieval. Reconciliation and
// history changes refresh only labels for an existing ID, preserving hydrated
// HTML, attachments, and reply headers already stored on this device.
fn retrieve_sync_message(
    client: &Client,
    token: &str,
    email: &str,
    id: &str,
    cancel: &AtomicBool,
    store: &Store,
    api: &str,
) -> Result<Option<Message>> {
    let cached = store.cached_message_optional(email, id)?;
    let format = if cached.is_some() { "minimal" } else { "full" };
    let pace = if cached.is_some() {
        Duration::from_millis(300)
    } else {
        Duration::from_millis(800)
    };
    let response = read_with_pacing(
        client
            .get(format!("{api}/messages/{id}?format={format}"))
            .bearer_auth(token),
        cancel,
        pace,
        |duration| pause(cancel, duration),
    )?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let value = json(response)?;
    anyhow::ensure!(
        value["id"].as_str() == Some(id),
        "Gmail returned a different message ID"
    );
    if let Some(mut cached) = cached {
        let labels = parse_message(&value, email)?;
        cached.labels = labels.labels;
        cached.folder = labels.folder;
        cached.unread = labels.unread;
        cached.flagged = labels.flagged;
        Ok(Some(cached))
    } else {
        Ok(Some(parse_message(&value, email)?))
    }
}

fn sync_with_store(
    client: &Client,
    token: &str,
    email: &str,
    cancel: &AtomicBool,
    store: &mut Store,
    api: &str,
) -> Result<()> {
    cancelled(cancel)?;
    store.resume_abandoned_sync(email)?;
    if store.full_sync_finalizing(email)? {
        store.finish_full_sync_cancellable(email, Some(cancel))?;
        return store.prune_sync_history(email);
    }
    let history = store.history(email)?;
    if !history.is_empty() && store.full_sync(email)?.is_none() {
        let response = read_request(
            client
                .get(format!("{api}/history"))
                .bearer_auth(token)
                .query(&[("startHistoryId", history.as_str()), ("maxResults", "500")]),
            cancel,
        )?;
        if response.status() != reqwest::StatusCode::NOT_FOUND {
            let mut page = json(response)?;
            let mut ids = std::collections::BTreeSet::new();
            let final_history = loop {
                cancelled(cancel)?;
                if let Some(records) = page["history"].as_array() {
                    for record in records {
                        if let Some(messages) = record["messages"].as_array() {
                            for message in messages {
                                ids.insert(
                                    message["id"]
                                        .as_str()
                                        .context("Missing history message ID")?
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
                if let Some(next) = page["nextPageToken"].as_str() {
                    page = json(read_request(
                        client
                            .get(format!("{api}/history"))
                            .bearer_auth(token)
                            .query(&[
                                ("startHistoryId", history.as_str()),
                                ("pageToken", next),
                                ("maxResults", "500"),
                            ]),
                        cancel,
                    )?)?;
                } else {
                    break page["historyId"]
                        .as_str()
                        .context("Missing history ID")?
                        .to_string();
                }
            };
            let mut changes = Vec::new();
            let changed_total = ids.len();
            store.sync_progress(email, "updating", 0, Some(changed_total))?;
            for (completed, id) in ids.into_iter().enumerate() {
                cancelled(cancel)?;
                let message = retrieve_sync_message(client, token, email, &id, cancel, store, api)?;
                changes.push((id, message));
                store.sync_progress(email, "updating", completed + 1, Some(changed_total))?;
            }
            cancelled(cancel)?;
            store.apply_sync_changes(email, &changes, &final_history)?;
            return store.prune_sync_history(email);
        }
    }
    let phase = if store.has_committed_messages(email)? {
        "checking_mail"
    } else {
        "importing"
    };
    // Resume a partially downloaded page without fetching its completed messages again.
    let (_, mut next) = match store.full_sync(email)? {
        Some(state) => state,
        None => {
            let profile = json(read_request(
                client.get(format!("{api}/profile")).bearer_auth(token),
                cancel,
            )?)?;
            let history = profile["historyId"]
                .as_str()
                .context("Missing history ID")?;
            store.begin_full_sync(email, history)?;
            store.sync_progress(
                email,
                phase,
                0,
                if store.sync_history(email)? == crate::settings::SyncHistory::All {
                    profile["messagesTotal"].as_u64().map(|n| n as usize)
                } else {
                    None
                },
            )?;
            (history.to_string(), String::new())
        }
    };
    // Persisted with the checkpoint so resumed pages use exactly the same query.
    let query = store
        .scan_since(email)?
        .map(|since| format!("after:{}", since / 1000))
        .unwrap_or_default();
    let mut downloaded = store.staged_ids(email)?;
    store.sync_progress(email, phase, downloaded.len(), None)?;
    let mut recent_refresh = if downloaded.len() >= 500 {
        Instant::now()
    } else {
        Instant::now() + Duration::from_secs(60)
    };
    loop {
        cancelled(cancel)?;
        if Instant::now() >= recent_refresh {
            refresh_recent_inbox(client, token, email, cancel, store, api, &mut downloaded)?;
            recent_refresh = Instant::now() + Duration::from_secs(60);
        }
        let listed = json(read_request(
            client
                .get(format!("{api}/messages"))
                .bearer_auth(token)
                .query(&[
                    ("maxResults", "500"),
                    ("includeSpamTrash", "true"),
                    ("q", query.as_str()),
                    ("pageToken", next.as_str()),
                ]),
            cancel,
        )?)?;
        if let Some(ids) = listed["messages"].as_array() {
            for id in ids {
                cancelled(cancel)?;
                if Instant::now() >= recent_refresh {
                    refresh_recent_inbox(
                        client,
                        token,
                        email,
                        cancel,
                        store,
                        api,
                        &mut downloaded,
                    )?;
                    recent_refresh = Instant::now() + Duration::from_secs(60);
                }
                let id = id["id"].as_str().context("Gmail message missing ID")?;
                if downloaded.contains(id) {
                    continue;
                }
                if let Some(message) =
                    retrieve_sync_message(client, token, email, id, cancel, store, api)?
                {
                    store.stage_message(email, &message)?;
                    downloaded.insert(id.to_string());
                    store.sync_progress(email, phase, downloaded.len(), None)?;
                }
            }
        }
        match listed["nextPageToken"].as_str() {
            Some(page) => {
                next = page.into();
                store.checkpoint_page(email, &next)?;
            }
            None => break,
        }
    }
    cancelled(cancel)?;
    store.sync_progress(email, "saving", downloaded.len(), Some(downloaded.len()))?;
    store.finish_full_sync_cancellable(email, Some(cancel))?;
    store.prune_sync_history(email)
}

fn header(payload: &Value, name: &str) -> String {
    let raw = payload["headers"]
        .as_array()
        .and_then(|h| {
            h.iter().find(|h| {
                h["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|h| h["value"].as_str())
        .unwrap_or("")
        .to_string();
    let line = format!("{name}: {raw}");
    mailparse::parse_header(line.as_bytes())
        .map(|(h, _)| h.get_value())
        .unwrap_or(raw)
}
fn attachment_parts(payload: &Value, out: &mut Vec<Attachment>) {
    let name = payload["filename"].as_str().unwrap_or_default();
    if !name.is_empty() {
        out.push(Attachment {
            name: crate::attachments::filename(name),
            size: payload["body"]["size"].as_u64().unwrap_or_default(),
            id: payload["body"]["attachmentId"]
                .as_str()
                .unwrap_or_default()
                .into(),
            data: payload["body"]["data"].as_str().unwrap_or_default().into(),
        });
    }
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            attachment_parts(part, out);
        }
    }
}

pub fn load_content(message: &Message) -> Result<Message> {
    let client = client()?;
    let token = access_token(&client, &message.account)?;
    let mut value = json(read_request(
        client
            .get(format!("{API}/messages/{}?format=full", message.id))
            .bearer_auth(&token),
        &AtomicBool::new(false),
    )?)?;
    hydrate_bodies(&client, &token, &message.id, &mut value["payload"], 0, API)?;
    let mut message = parse_message(&value, &message.account)?;
    if let Some(doc) = &mut message.html {
        doc.inline_attempted = true;
    }
    Store::open()?.cache_content(&message)?;
    Ok(message)
}

fn hydrate_bodies(
    client: &Client,
    token: &str,
    message_id: &str,
    payload: &mut Value,
    depth: usize,
    api: &str,
) -> Result<()> {
    let mut remaining = HydrationBudget {
        bytes: crate::html_layout::MAX_IMAGE_TOTAL,
        images: 20,
    };
    hydrate_parts(
        client,
        token,
        message_id,
        payload,
        depth,
        api,
        &mut remaining,
    )
}
struct HydrationBudget {
    bytes: usize,
    images: usize,
}
fn hydrate_parts(
    client: &Client,
    token: &str,
    message_id: &str,
    payload: &mut Value,
    depth: usize,
    api: &str,
    remaining: &mut HydrationBudget,
) -> Result<()> {
    let inline = crate::html_layout::raster_type(payload["mimeType"].as_str().unwrap_or_default())
        && !header(payload, "Content-ID").is_empty();
    if depth > 64 || (payload["filename"].as_str().is_some_and(|n| !n.is_empty()) && !inline) {
        return Ok(());
    }
    if matches!(
        payload["mimeType"].as_str(),
        Some("text/html" | "text/plain")
    ) || inline
    {
        if let Some(id) = payload["body"]["attachmentId"].as_str() {
            let size = payload["body"]["size"].as_u64().unwrap_or_default() as usize;
            if inline
                && (size > crate::html_layout::MAX_INLINE_BYTES
                    || size > remaining.bytes
                    || remaining.images == 0)
            {
                return Ok(());
            }
            if inline {
                remaining.images -= 1;
            }
            anyhow::ensure!(
                size <= if inline {
                    crate::html_layout::MAX_INLINE_BYTES
                } else {
                    crate::html::MAX_HTML
                },
                "Message body exceeds the 1 MiB display limit"
            );
            let response = read_request(
                client
                    .get(format!("{api}/messages/{message_id}/attachments/{id}"))
                    .bearer_auth(token),
                &AtomicBool::new(false),
            )?;
            let body: Value = serde_json::from_reader(
                response.take((crate::html_layout::MAX_INLINE_BYTES * 2) as u64),
            )?;
            anyhow::ensure!(
                body["data"].is_string(),
                "Gmail returned no message body data"
            );
            if inline {
                let encoded = body["data"].as_str().unwrap_or_default();
                anyhow::ensure!(
                    encoded.len() <= crate::html_layout::MAX_INLINE_BYTES.div_ceil(3) * 4,
                    "Inline image exceeds the display limit"
                );
                let bytes = URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('='))?;
                anyhow::ensure!(
                    bytes.len() <= remaining.bytes
                        && bytes.len() <= crate::html_layout::MAX_INLINE_BYTES,
                    "Inline image exceeds the display budget"
                );
                remaining.bytes -= bytes.len();
            }
            payload["body"]["data"] = body["data"].clone();
            payload["body"]
                .as_object_mut()
                .context("Invalid Gmail body")?
                .remove("attachmentId");
        }
    }
    if let Some(parts) = payload["parts"].as_array_mut() {
        for part in parts {
            hydrate_parts(client, token, message_id, part, depth + 1, api, remaining)?;
        }
    }
    Ok(())
}

fn decode_attachment(attachment: &Attachment, data: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(
        attachment.size <= MAX_BYTES as u64 && data.len() <= MAX_BYTES.div_ceil(3) * 4,
        "Attachment exceeds the 20 MiB download limit"
    );
    anyhow::ensure!(
        attachment.size == 0 || !data.is_empty(),
        "Attachment data is unavailable; reload its attachment list"
    );
    let bytes = URL_SAFE_NO_PAD.decode(data.trim_end_matches('='))?;
    anyhow::ensure!(
        bytes.len() <= MAX_BYTES,
        "Attachment exceeds the 20 MiB download limit"
    );
    anyhow::ensure!(
        attachment.size == 0 || bytes.len() as u64 == attachment.size,
        "Attachment size did not match Gmail metadata"
    );
    Ok(bytes)
}

fn fetch_attachment(
    client: &Client,
    token: &str,
    message_id: &str,
    attachment: &Attachment,
    api: &str,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        attachment.size <= MAX_BYTES as u64,
        "Attachment exceeds the 20 MiB download limit"
    );
    let response = read_request(
        client
            .get(format!(
                "{api}/messages/{message_id}/attachments/{}",
                attachment.id
            ))
            .bearer_auth(token),
        &AtomicBool::new(false),
    )?;
    // Bound the encoded response before allocating/decoding attachment contents.
    let value: Value =
        serde_json::from_reader(response.take((MAX_BYTES.div_ceil(3) * 4 + 16384) as u64))?;
    decode_attachment(
        attachment,
        value["data"]
            .as_str()
            .context("Gmail returned no attachment data")?,
    )
}

pub fn forward_files(
    source: &crate::drafts::ForwardSource,
    selected: &[usize],
    remaining: usize,
) -> Result<Vec<DraftAttachment>> {
    let mut files = Vec::new();
    let mut used = 0;
    let client = client()?;
    let token = if selected
        .iter()
        .any(|i| source.attachments.get(*i).is_some_and(|a| !a.id.is_empty()))
    {
        access_token(&client, &source.account)?
    } else {
        String::new()
    };
    for index in selected {
        let attachment = source
            .attachments
            .get(*index)
            .context("Attachment no longer available")?;
        anyhow::ensure!(
            attachment.size <= remaining.saturating_sub(used) as u64,
            "Forwarded attachments exceed the remaining 20 MiB budget"
        );
        let data = if attachment.id.is_empty() {
            decode_attachment(attachment, &attachment.data)?
        } else {
            fetch_attachment(&client, &token, &source.message_id, attachment, API)?
        };
        used += data.len();
        anyhow::ensure!(
            used <= remaining,
            "Forwarded attachments exceed the remaining 20 MiB budget"
        );
        files.push(DraftAttachment {
            name: attachment.name.clone(),
            data,
        });
    }
    Ok(files)
}

pub fn download(message: &Message, attachment: &Attachment) -> Result<String> {
    let bytes = if attachment.id.is_empty() {
        decode_attachment(attachment, &attachment.data)?
    } else {
        let client = client()?;
        let token = access_token(&client, &message.account)?;
        fetch_attachment(&client, &token, &message.id, attachment, API)?
    };
    let directory = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|p| p.join("Downloads")))
        .context("Cannot locate Downloads directory")?;
    let path = crate::attachments::save_download(&directory, &attachment.name, &bytes)?;
    Ok(format!("Saved attachment to {}", path.display()))
}

fn body_parts(payload: &Value, out: &mut Vec<String>, mime: &str) {
    // Attachments (including text files) must never be treated as the message body.
    if payload["filename"].as_str().is_some_and(|s| !s.is_empty()) {
        return;
    }
    if payload["mimeType"] == mime {
        if let Some(data) = payload["body"]["data"].as_str() {
            if let Ok(bytes) = URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')) {
                // Gmail has already removed MIME transfer encoding. Preserve the
                // declared charset while decoding the base64url API envelope.
                let content_type = header(payload, "Content-Type").replace(['\r', '\n'], " ");
                let content_type = if content_type.is_empty() {
                    "text/plain; charset=utf-8"
                } else {
                    &content_type
                };
                let mut mime = format!(
                    "Content-Type: {content_type}\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"
                )
                .into_bytes();
                mime.extend_from_slice(&bytes);
                let text = mailparse::parse_mail(&mime)
                    .ok()
                    .and_then(|p| p.get_body().ok())
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
                out.push(text);
            }
        }
    }
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            body_parts(part, out, mime);
        }
    }
}
fn missing_body(payload: &Value, depth: usize) -> bool {
    if depth > 64 || payload["filename"].as_str().is_some_and(|s| !s.is_empty()) {
        return false;
    }
    (matches!(
        payload["mimeType"].as_str(),
        Some("text/html" | "text/plain")
    ) && payload["body"]["attachmentId"].is_string())
        || payload["parts"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|part| missing_body(part, depth + 1)))
}

fn gmail_inline_images(payload: &Value, doc: &mut crate::html::Document, depth: usize) {
    if depth > 64 {
        return;
    }
    let mime = payload["mimeType"].as_str().unwrap_or_default();
    if crate::html_layout::raster_type(mime) {
        let cid = header(payload, "Content-ID");
        if !cid.is_empty() {
            if let Some(data) = payload["body"]["data"]
                .as_str()
                .filter(|s| s.len() <= crate::html_layout::MAX_INLINE_BYTES.div_ceil(3) * 4)
            {
                if let Ok(bytes) = URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')) {
                    crate::html_layout::attach_inline(doc, &cid, mime, &bytes);
                }
            }
        }
    }
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            gmail_inline_images(part, doc, depth + 1);
        }
    }
}

fn parse_message(value: &Value, account: &str) -> Result<Message> {
    let payload = &value["payload"];
    let from = header(payload, "From");
    let parsed = mailparse::addrparse(&from).ok();
    let address = parsed.and_then(|list| list.extract_single_info());
    let (sender, email) = address
        .map(|a| (a.display_name.unwrap_or_else(|| a.addr.clone()), a.addr))
        .unwrap_or_else(|| (from.clone(), from));
    let mut attachments = Vec::new();
    attachment_parts(payload, &mut attachments);
    let mut parts = Vec::new();
    body_parts(payload, &mut parts, "text/plain");
    let mut html_parts = Vec::new();
    body_parts(payload, &mut html_parts, "text/html");
    let mut html = if html_parts.is_empty() {
        None
    } else {
        Some(crate::html::parse(&html_parts.join("\n")))
    };
    if let Some(doc) = &mut html {
        gmail_inline_images(payload, doc, 0);
    }
    let body = if parts.is_empty() {
        html.as_ref().map(|document| document.plain_text()).filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}\n\n[Message body unavailable. Load formatted message to retrieve available content.]", value["snippet"].as_str().unwrap_or("")))
    } else {
        parts.join("\n\n")
    };
    let labels = value["labelIds"].as_array();
    let has = |label: &str| labels.is_some_and(|l| l.iter().any(|v| v == label));
    Ok(Message {
        id: value["id"].as_str().context("Missing message ID")?.into(),
        sender,
        email,
        subject: header(payload, "Subject"),
        body,
        date: header(payload, "Date"),
        account: account.into(),
        folder: if has("TRASH") {
            "Trash"
        } else if has("SPAM") {
            "Spam"
        } else if has("DRAFT") {
            "Server Drafts"
        } else if has("INBOX") {
            "Inbox"
        } else if has("SENT") {
            "Sent"
        } else {
            "Archive"
        }
        .into(),
        unread: has("UNREAD"),
        flagged: has("STARRED"),
        attachments,
        attachments_loaded: true,
        html,
        html_loaded: !missing_body(payload, 0),
        to_header: header(payload, "To"),
        cc_header: header(payload, "Cc"),
        reply_to: header(payload, "Reply-To"),
        delivered_to: header(payload, "Delivered-To"),
        message_id: reference_ids(&header(payload, "Message-ID")).replace("\r\n ", " "),
        references: reference_ids(&header(payload, "References")).replace("\r\n ", " "),
        thread_id: value["threadId"].as_str().unwrap_or_default().into(),
        recipients_loaded: true,
        labels: labels
            .map(|l| {
                l.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        timestamp: value["internalDate"]
            .as_str()
            .and_then(|v| v.parse().ok())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mock(responses: Vec<(&'static str, u16, Value)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (expected, status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "Missing request: {expected}");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(
                    line.contains(expected.split('|').next().unwrap()),
                    "Expected {expected}, got {line}"
                );
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                }
                let mut request_body = vec![0; length];
                reader.read_exact(&mut request_body).unwrap();
                if let Some((_, expected_body)) = expected.split_once('|') {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&request_body).unwrap(),
                        serde_json::from_str::<Value>(expected_body).unwrap()
                    );
                }
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (address, worker)
    }

    fn fixture(id: &str, labels: &[&str], date: &str) -> Value {
        serde_json::json!({"id":id,"labelIds": labels,"internalDate":date,"payload":{"headers":[],"mimeType":"text/plain","body":{"data":"aGVsbG8"}}})
    }

    #[test]
    fn current_inbox_refresh_keeps_the_historical_import_cursor_and_cache() {
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        store
            .begin_full_sync("me@example.com", "original-history")
            .unwrap();
        store
            .checkpoint_page("me@example.com", "historical-page-9")
            .unwrap();
        let old = parse_message(
            &fixture("old", &["INBOX"], "1700000000000"),
            "me@example.com",
        )
        .unwrap();
        store.stage_message("me@example.com", &old).unwrap();
        let mut downloaded = store.staged_ids("me@example.com").unwrap();
        let (api, worker) = mock(vec![
            (
                "/messages?maxResults=50&labelIds=INBOX",
                200,
                serde_json::json!({"messages":[{"id":"latest"},{"id":"old"},{"id":"vanished"}]}),
            ),
            (
                "/messages/latest?format=full",
                200,
                fixture("latest", &["INBOX"], "1794000000000"),
            ),
            ("/messages/vanished?format=full", 404, serde_json::json!({})),
        ]);
        refresh_recent_inbox(
            &client().unwrap(),
            "test",
            "me@example.com",
            &AtomicBool::new(false),
            &store,
            &api,
            &mut downloaded,
        )
        .unwrap();
        worker.join().unwrap();
        assert_eq!(downloaded.len(), 2);
        assert_eq!(
            store.full_sync("me@example.com").unwrap(),
            Some(("original-history".into(), "historical-page-9".into()))
        );
        assert_eq!(store.history("me@example.com").unwrap(), "");
        let page = store.mailbox_page("All Inboxes", "", 0).unwrap();
        assert_eq!(page["total"], 2);
        assert_eq!(page["messages"][0]["id"], "latest");
        assert_eq!(page["messages"][1]["id"], "old");
    }

    #[test]
    fn revoked_refresh_is_distinct_from_temporary_token_errors() {
        let revoked = refresh_response(reqwest::StatusCode::BAD_REQUEST, serde_json::json!({"error":"invalid_grant","error_description":"sensitive provider text"})).err().unwrap();
        assert!(revoked.is::<crate::service::ReconnectRequired>());
        assert!(!revoked.to_string().contains("sensitive"));
        let transient = refresh_response(reqwest::StatusCode::SERVICE_UNAVAILABLE, Value::Null)
            .err()
            .unwrap();
        assert!(!transient.is::<crate::service::ReconnectRequired>());
    }

    #[test]
    fn reply_send_contains_cc_and_thread_headers_and_changed_subject_starts_new_thread() {
        use mailparse::MailHeaderMap;
        let draft = crate::drafts::Draft {
            id: "one".into(),
            from: "me@example.com".into(),
            to: "you@example.com".into(),
            cc: "other@example.com".into(),
            subject: "Re: Topic".into(),
            reply_subject: "Re: Topic".into(),
            reply_account: "me@example.com".into(),
            in_reply_to: "<original@example.com>".into(),
            references: "<ancestor@example.com> <original@example.com>".into(),
            thread_id: "thread".into(),
            ..Default::default()
        };
        let payload = draft_message(&draft, &[]).unwrap();
        assert_eq!(payload["threadId"], "thread");
        let bytes = URL_SAFE_NO_PAD
            .decode(payload["raw"].as_str().unwrap())
            .unwrap();
        let mail = mailparse::parse_mail(&bytes).unwrap();
        assert_eq!(
            mail.headers.get_first_value("Cc").unwrap(),
            "other@example.com"
        );
        assert_eq!(
            mail.headers.get_first_value("In-Reply-To").unwrap(),
            "<original@example.com>"
        );
        assert!(mail
            .headers
            .get_first_value("References")
            .unwrap()
            .contains("<ancestor@example.com>"));
        let mut changed = draft.clone();
        changed.subject = "New topic".into();
        assert!(draft_message(&changed, &[])
            .unwrap()
            .get("threadId")
            .is_none());
        changed = draft.clone();
        changed.from = "another@example.com".into();
        assert!(draft_message(&changed, &[])
            .unwrap()
            .get("threadId")
            .is_none());
        changed = draft;
        changed.cc = "copy@example.com\r\nBcc: bad@example.com".into();
        assert!(draft_message(&changed, &[]).is_err());
    }
    #[test]
    fn forward_attachment_selection_imports_only_selected_bytes_and_enforces_total_limit() {
        let source = crate::drafts::ForwardSource {
            account: "me@example.com".into(),
            message_id: "m".into(),
            attachments: vec![
                Attachment {
                    name: "one.txt".into(),
                    size: 3,
                    data: URL_SAFE_NO_PAD.encode("one"),
                    ..Default::default()
                },
                Attachment {
                    name: "two.txt".into(),
                    size: 3,
                    data: URL_SAFE_NO_PAD.encode("two"),
                    ..Default::default()
                },
            ],
        };
        let files = forward_files(&source, &[1], 3).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "two.txt");
        assert_eq!(files[0].data, b"two");
        assert!(forward_files(&source, &[0, 1], 5).is_err());
        assert!(forward_files(&source, &[99], 20).is_err());
    }

    #[test]
    fn mime_alternatives_preserve_plain_text_and_decode_formatted_charset() {
        let value = serde_json::json!({"id":"alternatives", "payload":{"mimeType":"multipart/alternative","parts":[
            {"mimeType":"text/plain","body":{"data":URL_SAFE_NO_PAD.encode("Plain version")}},
            {"mimeType":"text/html","headers":[{"name":"Content-Type","value":"text/html; charset=iso-8859-1"}],"body":{"data":URL_SAFE_NO_PAD.encode(b"<h2>Caf\xe9</h2><p><b>Bold</b></p><script>bad()</script>")}},
            {"mimeType":"text/html","filename":"attached.html","body":{"data":URL_SAFE_NO_PAD.encode("<p>Not the body</p>")}}
        ]}});
        let message = parse_message(&value, "me@example.com").unwrap();
        assert_eq!(message.body, "Plain version");
        let formatted = message.html.unwrap().plain_text();
        assert!(formatted.contains("Café"));
        assert!(!formatted.contains("Not the body"));
        assert!(!formatted.contains("bad()"));
        assert!(message.html_loaded);
    }

    #[test]
    fn gmail_cid_images_hydrate_without_fetching_unrelated_attachments() {
        let gif = base64::engine::general_purpose::STANDARD
            .decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")
            .unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(&gif);
        let (api, worker) = mock(vec![(
            "/messages/m/attachments/logo",
            200,
            serde_json::json!({"data":encoded}),
        )]);
        let mut payload = serde_json::json!({"mimeType":"multipart/related","parts":[
          {"mimeType":"text/html","body":{"data":URL_SAFE_NO_PAD.encode("<table><tr><td><img src='cid:logo'></td></tr></table>")}},
          {"mimeType":"image/gif","filename":"logo.gif","headers":[{"name":"Content-ID","value":"<logo>"}],"body":{"size":gif.len(),"attachmentId":"logo"}},
          {"mimeType":"application/pdf","filename":"unrelated.pdf","body":{"size":42,"attachmentId":"do-not-fetch"}}
        ]});
        hydrate_bodies(&client().unwrap(), "test", "m", &mut payload, 0, &api).unwrap();
        let message = parse_message(
            &serde_json::json!({"id":"m","payload":payload}),
            "me@example.com",
        )
        .unwrap();
        assert!(message.html.unwrap().images[0]
            .inline_data
            .as_ref()
            .unwrap()
            .starts_with("data:image/gif;base64,"));
        worker.join().unwrap();
        let mut oversized = serde_json::json!({"mimeType":"image/png","headers":[{"name":"Content-ID","value":"<huge>"}],"body":{"size":3*1024*1024,"attachmentId":"never"}});
        hydrate_bodies(
            &client().unwrap(),
            "test",
            "m",
            &mut oversized,
            0,
            "http://127.0.0.1:1",
        )
        .unwrap();
    }

    #[test]
    fn separate_gmail_html_body_loads_without_fetching_attached_files() {
        let mut payload = serde_json::json!({"mimeType":"multipart/mixed","parts":[
            {"mimeType":"text/html","body":{"attachmentId":"body","size":15}},
            {"mimeType":"text/html","filename":"file.html","body":{"attachmentId":"file","size":10}}
        ]});
        assert!(missing_body(&payload, 0));
        let (api, worker) = mock(vec![(
            "/messages/m/attachments/body",
            200,
            serde_json::json!({"data":URL_SAFE_NO_PAD.encode("<p>Readable</p>")}),
        )]);
        hydrate_bodies(&client().unwrap(), "test", "m", &mut payload, 0, &api).unwrap();
        assert!(!missing_body(&payload, 0));
        assert_eq!(payload["parts"][1]["body"]["attachmentId"], "file");
        let message = parse_message(
            &serde_json::json!({"id":"m","payload":payload}),
            "me@example.com",
        )
        .unwrap();
        assert_eq!(message.body, "Readable");
        worker.join().unwrap();
    }

    #[test]
    fn multipart_attachments_roundtrip_binary_unicode_names_and_empty_files() {
        let files = vec![
            DraftAttachment {
                name: "Résumé 📨.bin".into(),
                data: vec![0, 255, 13, 10, 128],
            },
            DraftAttachment {
                name: "empty.txt".into(),
                data: vec![],
            },
        ];
        let bytes = URL_SAFE_NO_PAD
            .decode(
                raw_message(
                    "me@example.com",
                    "you@example.com",
                    "Subject",
                    "Body",
                    &files,
                )
                .unwrap(),
            )
            .unwrap();
        let mail = mailparse::parse_mail(&bytes).unwrap();
        assert_eq!(mail.subparts.len(), 3);
        assert_eq!(mail.subparts[0].get_body().unwrap(), "Body");
        assert_eq!(mail.subparts[1].get_body_raw().unwrap(), files[0].data);
        assert_eq!(
            mail.subparts[1].get_content_disposition().params["filename"],
            files[0].name
        );
        assert_eq!(mail.subparts[2].get_body_raw().unwrap(), Vec::<u8>::new());
        let malicious = [DraftAttachment {
            name: "bad\r\nBcc: bad@example.com".into(),
            data: vec![],
        }];
        let raw = URL_SAFE_NO_PAD
            .decode(raw_message("me@example.com", "you@example.com", "Hi", "", &malicious).unwrap())
            .unwrap();
        assert!(!String::from_utf8(raw).unwrap().contains("\r\nBcc:"));
    }

    #[test]
    fn nested_attachment_metadata_and_remote_bytes_are_preserved() {
        use serde_json::json;
        let value = json!({"id":"m","payload":{"mimeType":"multipart/mixed","parts":[
            {"mimeType":"text/plain","body":{"data":"Ym9keQ"}},
            {"mimeType":"multipart/mixed","parts":[
                {"filename":"../../file.bin","body":{"attachmentId":"remote","size":3}},
                {"filename":"inline.txt","body":{"data":"aGk=","size":2}}
            ]}
        ]}});
        let message = parse_message(&value, "me@example.com").unwrap();
        assert_eq!(message.body, "body");
        assert_eq!(message.attachments.len(), 2);
        assert_eq!(message.attachments[0].name, "file.bin");
        assert_eq!(
            decode_attachment(&message.attachments[1], &message.attachments[1].data).unwrap(),
            b"hi"
        );
        let (api, worker) = mock(vec![(
            "/messages/m/attachments/remote",
            200,
            json!({"data":"AP-A","size":3}),
        )]);
        assert_eq!(
            fetch_attachment(
                &client().unwrap(),
                "test",
                "m",
                &message.attachments[0],
                &api
            )
            .unwrap(),
            vec![0, 255, 128]
        );
        worker.join().unwrap();
        assert!(decode_attachment(&message.attachments[0], "aGk").is_err());
        assert!(decode_attachment(
            &Attachment {
                size: MAX_BYTES as u64 + 1,
                ..Default::default()
            },
            ""
        )
        .is_err());
    }

    #[test]
    fn partial_mail_is_visible_and_resumes_without_redownloading_completed_messages() {
        use serde_json::json;
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        store.snapshot("me@example.com", &[], "").unwrap();
        store
            .set_sync_history("me@example.com", crate::settings::SyncHistory::All)
            .unwrap();
        let original = parse_message(&fixture("old", &["INBOX"], "1"), "me@example.com").unwrap();
        store.snapshot("me@example.com", &[original], "").unwrap();
        let (api, worker) = mock(vec![
            ("/profile", 200, json!({"historyId":"10"})),
            (
                "/messages?",
                200,
                json!({"messages":[{"id":"a"},{"id":"b"}]}),
            ),
            (
                "/messages/a?format=full",
                200,
                fixture("a", &["INBOX"], "200"),
            ),
            (
                "/messages/b?format=full",
                403,
                json!({"error":{"errors":[{"reason":"dailyLimitExceeded"}]}}),
            ),
            (
                "/messages?",
                200,
                json!({"messages":[{"id":"a"},{"id":"b"}]}),
            ),
            (
                "/messages/b?format=full",
                200,
                fixture("b", &["SENT"], "100"),
            ),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).is_err()
        );
        assert_eq!(
            store
                .load()
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "old"]
        );
        assert_eq!(store.history("me@example.com").unwrap(), "");
        assert!(store.full_sync("me@example.com").unwrap().is_some());
        sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).unwrap();
        assert_eq!(
            store
                .load()
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(store.history("me@example.com").unwrap(), "10");
        assert!(store.full_sync("me@example.com").unwrap().is_none());
        worker.join().unwrap();
    }

    #[test]
    fn reads_back_off_for_rate_limits_but_not_permissions_or_daily_quota() {
        use serde_json::json;
        let (api, worker) = mock(vec![
            (
                "/read",
                403,
                json!({"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}),
            ),
            ("/read", 429, json!({})),
            ("/read", 200, json!({"ok":true})),
            (
                "/denied",
                403,
                json!({"error":{"errors":[{"reason":"insufficientPermissions"}]}}),
            ),
            (
                "/daily",
                403,
                json!({"error":{"errors":[{"reason":"dailyLimitExceeded"}]}}),
            ),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        let mut waits = Vec::new();
        let response = read_with_wait(client.get(format!("{api}/read")), &cancel, |duration| {
            waits.push(duration);
            Ok(())
        })
        .unwrap();
        assert_eq!(json(response).unwrap()["ok"], true);
        assert_eq!(waits.len(), 5);
        assert!(waits[1] >= Duration::from_secs(1));
        assert!(waits[3] >= Duration::from_secs(2));
        for path in ["denied", "daily"] {
            let mut attempts = 0;
            assert!(
                read_with_wait(client.get(format!("{api}/{path}")), &cancel, |_| {
                    attempts += 1;
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(attempts, 1);
        }
        worker.join().unwrap();
        assert!(pause(&AtomicBool::new(true), Duration::from_secs(60)).is_err());
    }

    #[test]
    fn google_errors_explain_scope_service_and_quota_without_exposing_bodies() {
        use serde_json::json;
        for (reason, expected) in [
            ("insufficientPermissions", "Reconnect"),
            ("accessNotConfigured", "Enable Gmail API"),
            ("userRateLimitExceeded", "quotas"),
            ("domainPolicy", "administrator"),
        ] {
            assert!(error_guidance(
                &json!({"error":{"errors":[{"reason":reason}],"message":"private data"}})
            )
            .contains(expected));
        }
        assert!(error_guidance(
            &json!({"error":{"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}})
        )
        .contains("gmail.modify"));
        assert!(
            !error_guidance(&json!({"error":{"message":"private token"}}))
                .contains("private token")
        );
    }

    #[test]
    fn reconciliation_and_history_keep_cached_bodies_and_only_download_new_ids() {
        use serde_json::json;
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let email = "me@example.com";
        let mut cached = parse_message(&fixture("cached", &["INBOX"], "200"), email).unwrap();
        cached.body = "Previously downloaded body".into();
        cached.html = Some(crate::html::parse("<p>Hydrated HTML stays cached</p>"));
        cached.html_loaded = true;
        cached.reply_to = "reply@example.com".into();
        cached.attachments = vec![Attachment {
            id: "file".into(),
            name: "kept.bin".into(),
            size: 3,
            data: "YWJj".into(),
        }];
        let removed = parse_message(&fixture("removed", &["INBOX"], "100"), email).unwrap();
        store
            .snapshot(email, &[cached.clone(), removed], "")
            .unwrap();
        store
            .set_sync_history(email, crate::settings::SyncHistory::All)
            .unwrap();
        let (api, worker) = mock(vec![
            ("/profile", 200, json!({"historyId":"10"})),
            (
                "/messages?",
                200,
                json!({"messages":[{"id":"cached"},{"id":"new"}]}),
            ),
            (
                "/messages/cached?format=minimal",
                200,
                json!({"id":"cached","labelIds":["SENT","STARRED"]}),
            ),
            (
                "/messages/new?format=full",
                200,
                fixture("new", &["INBOX"], "300"),
            ),
            (
                "startHistoryId=10",
                200,
                json!({"history":[{"messages":[{"id":"cached"}]}],"historyId":"20"}),
            ),
            (
                "/messages/cached?format=minimal",
                200,
                json!({"id":"cached","labelIds":["TRASH"]}),
            ),
            ("startHistoryId=20", 200, json!({"historyId":"21"})),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        sync_with_store(&client, "test", email, &cancel, &mut store, &api).unwrap();
        let kept = store.cached_message(email, "cached").unwrap();
        assert_eq!(kept.body, cached.body);
        assert_eq!(kept.reply_to, cached.reply_to);
        assert_eq!(kept.attachments[0].data, "YWJj");
        assert!(kept.html.unwrap().plain_text().contains("Hydrated HTML"));
        assert_eq!(kept.folder, "Sent");
        assert!(kept.flagged);
        assert!(store
            .cached_message_optional(email, "removed")
            .unwrap()
            .is_none());
        sync_with_store(&client, "test", email, &cancel, &mut store, &api).unwrap();
        let kept = store.cached_message(email, "cached").unwrap();
        assert_eq!(kept.body, cached.body);
        assert_eq!(kept.attachments[0].data, "YWJj");
        assert_eq!(kept.folder, "Trash");
        sync_with_store(&client, "test", email, &cancel, &mut store, &api).unwrap();
        assert_eq!(store.history(email).unwrap(), "21");
        worker.join().unwrap();
    }

    #[test]
    fn default_gmail_import_filters_history_and_preserves_cutoff_on_resume() {
        use serde_json::json;
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let (api, worker) = mock(vec![
            ("/profile", 200, json!({"historyId":"10"})),
            ("q=after%3A", 200, json!({"nextPageToken":"second"})),
            ("pageToken=second", 403, json!({})),
            ("q=after%3A", 200, json!({})),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).is_err()
        );
        let since = store.scan_since("me@example.com").unwrap().unwrap();
        assert!((since - crate::settings::SyncHistory::OneYear.cutoff().unwrap()).abs() < 5000);
        assert_eq!(
            store.full_sync("me@example.com").unwrap().unwrap().1,
            "second"
        );
        sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).unwrap();
        assert_eq!(store.history("me@example.com").unwrap(), "10");
        worker.join().unwrap();
    }

    #[test]
    fn full_sync_paginates_then_history_applies_changes_and_deletions() {
        use serde_json::json;
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        store.snapshot("me@example.com", &[], "").unwrap();
        store
            .set_sync_history("me@example.com", crate::settings::SyncHistory::All)
            .unwrap();
        let (api, worker) = mock(vec![
            ("/profile", 200, json!({"historyId":"10"})),
            (
                "includeSpamTrash=true",
                200,
                json!({"messages":[{"id":"a"}],"nextPageToken":"next page"}),
            ),
            (
                "/messages/a?format=full",
                200,
                fixture("a", &["INBOX", "SENT"], "200"),
            ),
            ("pageToken=next+page", 200, json!({"messages":[{"id":"b"}]})),
            (
                "/messages/b?format=full",
                200,
                fixture("b", &["TRASH"], "100"),
            ),
            (
                "startHistoryId=10",
                200,
                json!({"history":[{"messages":[{"id":"a"},{"id":"b"}]}],"nextPageToken":"h2"}),
            ),
            (
                "pageToken=h2",
                200,
                json!({"history":[{"messages":[{"id":"c"}]}],"historyId":"20"}),
            ),
            (
                "/messages/a?format=minimal",
                200,
                fixture("a", &["STARRED"], "200"),
            ),
            ("/messages/b?format=minimal", 404, json!({})),
            (
                "/messages/c?format=full",
                200,
                fixture("c", &["SENT"], "300"),
            ),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).unwrap();
        let messages = store.load().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].in_folder("Inbox") && messages[0].in_folder("Sent"));
        assert_eq!(messages[1].folder, "Trash");
        sync_with_store(&client, "test", "me@example.com", &cancel, &mut store, &api).unwrap();
        let messages = store.load().unwrap();
        assert_eq!(
            messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["c", "a"]
        );
        assert_eq!(messages[1].folder, "Archive");
        assert!(messages[1].flagged);
        assert_eq!(store.history("me@example.com").unwrap(), "20");
        worker.join().unwrap();
    }

    #[test]
    fn expired_history_falls_back_and_failed_fetch_preserves_cache_and_cursor() {
        use serde_json::json;
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let original =
            parse_message(&fixture("original", &["INBOX"], "1"), "me@example.com").unwrap();
        store
            .snapshot("me@example.com", &[original], "old")
            .unwrap();
        let (api, worker) = mock(vec![
            ("startHistoryId=old", 404, json!({})),
            ("/profile", 200, json!({"historyId":"new"})),
            (
                "/messages?",
                200,
                json!({"messages":[{"id":"replacement"}]}),
            ),
            ("/messages/replacement?format=full", 503, json!({})),
        ]);
        assert!(sync_with_store(
            &client().unwrap(),
            "test",
            "me@example.com",
            &AtomicBool::new(false),
            &mut store,
            &api
        )
        .is_err());
        assert_eq!(store.load().unwrap()[0].id, "original");
        assert_eq!(store.history("me@example.com").unwrap(), "old");
        worker.join().unwrap();
    }

    #[test]
    fn actions_use_gmail_labels_and_send_is_not_retried_on_failure() {
        use serde_json::json;
        let (api, worker) = mock(vec![
            (
                "POST /messages/a/modify|{\"addLabelIds\":[],\"removeLabelIds\":[\"UNREAD\"]}",
                200,
                json!({}),
            ),
            (
                "POST /messages/a/modify|{\"addLabelIds\":[\"UNREAD\"],\"removeLabelIds\":[]}",
                200,
                json!({}),
            ),
            (
                "POST /messages/a/modify|{\"addLabelIds\":[\"STARRED\"],\"removeLabelIds\":[]}",
                200,
                json!({}),
            ),
            (
                "POST /messages/a/modify|{\"addLabelIds\":[],\"removeLabelIds\":[\"STARRED\"]}",
                200,
                json!({}),
            ),
            (
                "POST /messages/a/modify|{\"addLabelIds\":[],\"removeLabelIds\":[\"INBOX\"]}",
                200,
                json!({}),
            ),
            ("POST /messages/a/trash", 200, json!({})),
            ("POST /messages/send|{\"raw\":\"encoded\"}", 503, json!({})),
            (
                "POST /messages/send|{\"raw\":\"encoded\"}",
                200,
                json!({"id":"sent"}),
            ),
        ]);
        let client = client().unwrap();
        for action in [
            Action::Read(true),
            Action::Read(false),
            Action::Star(true),
            Action::Star(false),
            Action::Archive,
            Action::Trash,
        ] {
            mutate(&client, "test", "a", action, &api).unwrap();
        }
        assert!(deliver(&client, "test", "encoded", &api).is_err());
        deliver(&client, "test", "encoded", &api).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn outgoing_mail_roundtrips_unicode_and_rejects_header_injection() {
        use mailparse::MailHeaderMap;
        let raw = raw_message(
            "me@example.com",
            "You <you@example.com>",
            "Hello ☀",
            "First line\nCafé ☀",
            &[],
        )
        .unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(raw).unwrap();
        let parsed = mailparse::parse_mail(&bytes).unwrap();
        assert_eq!(
            parsed.headers.get_first_value("Subject").unwrap(),
            "Hello ☀"
        );
        assert_eq!(parsed.get_body().unwrap(), "First line\r\nCafé ☀");
        assert!(raw_message(
            "me@example.com",
            "you@example.com\r\nBcc: bad@example.com",
            "hello",
            "body",
            &[]
        )
        .is_err());
        assert!(raw_message("me@example.com", "", "hello", "body", &[]).is_err());
        assert!(raw_message(
            "me@example.com",
            "you@example.com",
            "hello\nBcc: bad@example.com",
            "body",
            &[]
        )
        .is_err());
    }

    #[test]
    fn callback_checks_state_path_and_duplicate_parameters() {
        assert_eq!(
            callback("GET /?code=abc&state=good HTTP/1.1", "good")
                .unwrap()
                .unwrap(),
            "abc"
        );
        assert!(
            callback("GET /?error=access_denied&state=good HTTP/1.1", "good")
                .unwrap()
                .is_none()
        );
        for line in [
            "GET /?code=abc&state=bad HTTP/1.1",
            "GET /evil?code=abc&state=good HTTP/1.1",
            "GET /?code=abc&state=good&state=bad HTTP/1.1",
        ] {
            assert!(callback(line, "good").is_err());
        }
    }
    #[test]
    fn multipart_plain_text_excludes_attachments_and_html() {
        let value = serde_json::json!({"id":"123", "labelIds":["INBOX","UNREAD"], "payload":{"headers":[{"name":"from","value":"Alice <alice@example.com>"},{"name":"Subject","value":"Hello"}], "parts":[{"mimeType":"text/html","body":{"data":URL_SAFE_NO_PAD.encode("<script>bad()</script>")}}, {"mimeType":"text/plain","body":{"data":URL_SAFE_NO_PAD.encode("Safe plain text")}}, {"mimeType":"text/plain","filename":"private.txt","body":{"data":URL_SAFE_NO_PAD.encode("attachment")}}]}});
        let message = parse_message(&value, "me@example.com").unwrap();
        assert_eq!(message.body, "Safe plain text");
        assert_eq!(message.email, "alice@example.com");
        assert!(message.unread);
        assert!(!message.flagged);
    }

    #[test]
    fn decodes_encoded_headers_and_non_utf8_text() {
        let value = serde_json::json!({"id":"charset", "payload": {
            "mimeType":"text/plain", "headers":[
                {"name":"From","value":"=?UTF-8?B?Sm9zw6k=?= <jose@example.com>"},
                {"name":"Subject","value":"=?UTF-8?B?Q2Fmw6k=?="},
                {"name":"Content-Type","value":"text/plain; charset=iso-8859-1"}
            ], "body":{"data":URL_SAFE_NO_PAD.encode(b"caf\xe9")}
        }});
        let message = parse_message(&value, "me@example.com").unwrap();
        assert_eq!(message.sender, "José");
        assert_eq!(message.subject, "Café");
        assert_eq!(message.body, "café");
    }

    #[test]
    fn html_only_messages_remain_inert_text() {
        let value = serde_json::json!({"id":"html", "snippet":"Preview only", "payload":{
            "mimeType":"text/html", "body":{"data":URL_SAFE_NO_PAD.encode("<img src='https://example.com/tracker'><script>bad()</script>")}
        }});
        let message = parse_message(&value, "me@example.com").unwrap();
        assert!(message.html.is_some());
        assert!(message.body.contains("Image blocked"));
        assert!(!message.body.contains("tracker"));
        assert!(!message.body.contains("<script>"));
    }
}
