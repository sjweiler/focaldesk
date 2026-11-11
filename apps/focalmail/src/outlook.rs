use crate::{
    providers::{cancelled, Action},
    settings::Profile,
    store::Store,
    Message,
};
use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
const API: &str = "https://graph.microsoft.com/v1.0";
const SCOPE: &str = "offline_access User.Read Mail.ReadWrite Mail.Send";
pub const DEVICE_AUTHORIZATION_PREFIX: &str = "focalmail:device-authorization:";
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub expires_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
}
fn device_authorization_prompt(device: &Value) -> Result<DeviceAuthorization> {
    let user_code = device["user_code"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .context("Microsoft returned no usable sign-in code; retry connecting the account")?;
    Ok(DeviceAuthorization {
        user_code: user_code.into(),
        expires_in: device["expires_in"].as_u64().unwrap_or(900).clamp(1, 900),
        verification_uri: None,
    })
}
fn microsoft_error_detail(body: &Value) -> String {
    let code = body["error"]["code"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("unknown_error");
    let message = body["error"]["message"]
        .as_str()
        .or_else(|| body["error_description"].as_str())
        .unwrap_or("Microsoft returned no further details");
    let clean = |text: &str, max: usize| {
        text.chars()
            .filter(|c| !c.is_control())
            .take(max)
            .collect::<String>()
    };
    let codes = body["error_codes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .take(8)
        .map(|c| format!("AADSTS{c}"))
        .collect::<Vec<_>>();
    // Return selected error fields only. Token/device-code responses may contain
    // credentials, which must never be included in an error or a log message.
    let first_line = message.lines().next().unwrap_or(message);
    format!(
        "{}{}: {}",
        clean(code, 128),
        if codes.is_empty() {
            String::new()
        } else {
            format!(" ({})", codes.join(", "))
        },
        clean(first_line, 800)
    )
}
fn validate_device_authorization(status: reqwest::StatusCode, body: &Value) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let detail = match body["error"].as_str() {
        Some("invalid_client" | "unauthorized_client") => "Check the application client ID and enable public client/device-code flows in its Microsoft app registration.",
        Some("invalid_scope") => "The Microsoft app registration does not allow the requested mail permissions.",
        _ => "Check the Microsoft tenant, application configuration, and your organization's sign-in policy.",
    };
    bail!(
        "Microsoft could not issue a sign-in code ({status}). {detail} {}",
        microsoft_error_detail(body)
    )
}
#[derive(Serialize, Deserialize)]
struct Credential {
    refresh_token: String,
}
fn entry(email: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        "org.focaldesk.FocalMail.outlook",
        email,
    )?)
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn authority(p: &Profile) -> Result<String> {
    anyhow::ensure!(
        !p.client_id.trim().is_empty(),
        "Enter a Microsoft application client ID"
    );
    anyhow::ensure!(
        !p.tenant.is_empty()
            && p.tenant
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
        "Invalid Microsoft tenant"
    );
    Ok(format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0",
        p.tenant
    ))
}
fn checked(response: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let claims_required = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("insufficient_claims"));
    let body: Value = serde_json::from_reader(response.take(16 * 1024)).unwrap_or(Value::Null);
    let detail = microsoft_error_detail(&body);
    if status == reqwest::StatusCode::UNAUTHORIZED && !claims_required {
        return Err(
            anyhow::Error::new(crate::service::ReconnectRequired).context(format!(
                "Microsoft Graph rejected the app's authorization ({status}). {detail}"
            )),
        );
    }
    bail!("Microsoft Graph denied the requested operation ({status}). {detail}")
}

fn wait(cancel: &AtomicBool, seconds: u64) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        cancelled(cancel)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
pub fn connect(p: &Profile, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
    super::providers::ensure_provider(p)?;
    let client = client()?;
    let authority = authority(p)?;
    let token = match p.microsoft_sign_in {
        crate::settings::MicrosoftSignIn::Browser => {
            authorize_browser(p, &client, &authority, &cancel, &progress)?
        }
        crate::settings::MicrosoftSignIn::DeviceCode => {
            authorize_device_code(p, &client, &authority, &cancel, &progress)?
        }
    };
    cancelled(&cancel)?;
    let access = token["access_token"]
        .as_str()
        .context("Missing access token")?;
    let me: Value = checked(
        client
            .get(format!("{API}/me?$select=mail,userPrincipalName"))
            .bearer_auth(access)
            .send()?,
    )?
    .json()?;
    let email = me["mail"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| me["userPrincipalName"].as_str())
        .context("Microsoft did not return a mailbox address")?;
    anyhow::ensure!(
        email.eq_ignore_ascii_case(&p.email),
        "Signed-in Microsoft address does not match this account"
    );
    let saved = Credential {
        refresh_token: token["refresh_token"]
            .as_str()
            .context("No refresh authorization; grant offline_access")?
            .into(),
    };
    entry(&p.email)?.set_password(&serde_json::to_string(&saved)?)?;
    Store::open()?.connect_profile(p)?;
    Store::open()?.record_status(&p.email, &Ok(()))?;
    progress("Microsoft account authorized. Account connected.");
    Ok(format!("Connected {}", p.email))
}
fn authorize_device_code(
    p: &Profile,
    client: &Client,
    authority: &str,
    cancel: &AtomicBool,
    progress: &impl Fn(&str),
) -> Result<Value> {
    let response = client
        .post(format!("{authority}/devicecode"))
        .form(&[("client_id", p.client_id.as_str()), ("scope", SCOPE)])
        .send()?;
    let status = response.status();
    let device: Value = response.json()?;
    validate_device_authorization(status, &device)?;
    let code = device["device_code"]
        .as_str()
        .context("Missing device code")?;
    let prompt = device_authorization_prompt(&device)?;
    progress(&format!(
        "{DEVICE_AUTHORIZATION_PREFIX}{}",
        serde_json::to_string(&prompt)?
    ));
    // Show the code before the user chooses to open the verification page.
    let deadline = Instant::now() + Duration::from_secs(prompt.expires_in);
    let mut interval = device["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let token = loop {
        wait(cancel, interval)?;
        anyhow::ensure!(Instant::now() < deadline, "Microsoft sign-in timed out");
        let response = client
            .post(format!("{authority}/token"))
            .form(&[
                ("client_id", p.client_id.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", code),
            ])
            .send()?;
        let status = response.status();
        let body: Value = response.json()?;
        if status.is_success() {
            break body;
        }
        match body["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = (interval + 5).min(60),
            Some("expired_token") => bail!("Microsoft sign-in code expired. Close this dialog and connect again for a new code."),
            _ => bail!("Microsoft could not authorize this app: {}", microsoft_error_detail(&body)),
        }
    };
    Ok(token)
}

fn token(p: &Profile, client: &Client) -> Result<String> {
    let saved: Credential = serde_json::from_str(
        &entry(&p.email)?
            .get_password()
            .map_err(crate::service::credential_error)?,
    )?;
    let response = client
        .post(format!("{}/token", authority(p)?))
        .form(&[
            ("client_id", p.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", saved.refresh_token.as_str()),
            ("scope", SCOPE),
        ])
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    validate_refresh(status, &body)?;
    if let Some(refresh) = body["refresh_token"].as_str() {
        entry(&p.email)?.set_password(&serde_json::to_string(&Credential {
            refresh_token: refresh.into(),
        })?)?;
    }
    Ok(body["access_token"]
        .as_str()
        .context("Missing access token")?
        .into())
}

fn browser_authorization_url(
    p: &Profile,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> Result<url::Url> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let mut url = url::Url::parse(&format!("{}/authorize", authority(p)?))?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut().extend_pairs([
        ("client_id", p.client_id.as_str()),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("response_mode", "query"),
        ("scope", SCOPE),
        ("prompt", "select_account"),
        ("login_hint", p.email.as_str()),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge.as_str()),
        ("state", state),
    ]);
    Ok(url)
}

enum BrowserCallback {
    Code(String),
    Denied(String),
}
fn browser_callback(line: &str, expected: &str) -> Result<BrowserCallback> {
    let mut parts = line.split_whitespace();
    anyhow::ensure!(
        parts.next() == Some("GET"),
        "Invalid Microsoft callback method"
    );
    let target = parts.next().context("Missing Microsoft callback target")?;
    anyhow::ensure!(target.starts_with("/?"), "Invalid Microsoft callback path");
    let url = url::Url::parse(&format!("http://localhost{target}"))?;
    let values = url.query_pairs().collect::<Vec<_>>();
    let states = values
        .iter()
        .filter(|(key, _)| key == "state")
        .collect::<Vec<_>>();
    anyhow::ensure!(
        states.len() == 1 && states[0].1 == expected,
        "Microsoft OAuth state mismatch"
    );
    let errors = values
        .iter()
        .filter(|(key, _)| key == "error")
        .collect::<Vec<_>>();
    let codes = values
        .iter()
        .filter(|(key, _)| key == "code")
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        anyhow::ensure!(
            errors.len() == 1 && codes.is_empty(),
            "Ambiguous Microsoft callback"
        );
        let description = values
            .iter()
            .find(|(key, _)| key == "error_description")
            .map(|(_, v)| v.as_ref())
            .unwrap_or("Microsoft denied this authorization request");
        return Ok(BrowserCallback::Denied(microsoft_error_detail(
            &json!({"error":errors[0].1,"error_description":description}),
        )));
    }
    anyhow::ensure!(
        codes.len() == 1 && !codes[0].1.is_empty(),
        "Missing Microsoft authorization code"
    );
    Ok(BrowserCallback::Code(codes[0].1.to_string()))
}

fn authorize_browser(
    p: &Profile,
    client: &Client,
    authority: &str,
    cancel: &AtomicBool,
    progress: &impl Fn(&str),
) -> Result<Value> {
    use base64::Engine;
    use rand::RngCore;
    let random = || {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    };
    // Match the localhost public-client redirect used by FocalDesk Calendar.
    // Bind before opening the browser; verifier and state exist only in memory.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let redirect = format!("http://localhost:{}/", listener.local_addr()?.port());
    let verifier = random();
    let state = random();
    let url = browser_authorization_url(p, &redirect, &state, &verifier)?;
    let prompt = DeviceAuthorization {
        user_code: String::new(),
        expires_in: 600,
        verification_uri: Some(url.to_string()),
    };
    progress(&format!(
        "{DEVICE_AUTHORIZATION_PREFIX}{}",
        serde_json::to_string(&prompt)?
    ));
    if webbrowser::open(url.as_str()).is_err() {
        progress("Use Open Microsoft sign-in in the dialog to open your browser.");
    } else {
        progress("Finish Microsoft sign-in and MFA in your browser, then return to FocalMail.");
    }
    let deadline = Instant::now() + Duration::from_secs(600);
    let code = loop {
        cancelled(cancel)?;
        anyhow::ensure!(
            Instant::now() < deadline,
            "Microsoft browser sign-in timed out. Close the dialog and try again."
        );
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut line = String::new();
                if BufReader::new((&stream).take(8192))
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let result = browser_callback(&line, &state);
                let text = if result.is_ok() {
                    "Microsoft response received. Return to FocalMail to complete account verification."
                } else {
                    "Invalid authorization response. Return to FocalMail."
                };
                let status = if result.is_ok() {
                    "200 OK"
                } else {
                    "400 Bad Request"
                };
                let _=write!(stream,"HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{text}",text.len());
                match result {
                    Ok(BrowserCallback::Code(code)) => break code,
                    Ok(BrowserCallback::Denied(detail)) => {
                        bail!("Microsoft denied browser authorization: {detail}")
                    }
                    Err(_) => continue,
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(error) => return Err(error.into()),
        }
    };
    cancelled(cancel)?;
    progress("Microsoft browser response received. Verifying account authorization…");
    let response = client
        .post(format!("{authority}/token"))
        .form(&[
            ("client_id", p.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
            ("scope", SCOPE),
        ])
        .send()?;
    let status = response.status();
    let body: Value = response.json()?;
    anyhow::ensure!(
        status.is_success(),
        "Microsoft browser token exchange failed ({status}). {}",
        microsoft_error_detail(&body)
    );
    Ok(body)
}
fn validate_refresh(status: reqwest::StatusCode, body: &Value) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED
        || matches!(
            body["error"].as_str(),
            Some("invalid_grant" | "interaction_required" | "login_required" | "consent_required")
        )
    {
        return Err(
            anyhow::Error::new(crate::service::ReconnectRequired).context(format!(
                "Microsoft requires authorization for this app: {}",
                microsoft_error_detail(body)
            )),
        );
    }
    anyhow::ensure!(
        status.is_success(),
        "Microsoft token refresh failed ({status}). {}",
        microsoft_error_detail(body)
    );
    Ok(())
}
fn endpoint(path: &str) -> String {
    format!("{API}{path}")
}
fn message_url(id: &str) -> Result<url::Url> {
    let mut u = url::Url::parse(&endpoint("/me/messages/"))?;
    u.path_segments_mut().unwrap().push(id);
    Ok(u)
}
fn trusted_page(url: &str, api: &str) -> Result<()> {
    let u = url::Url::parse(url)?;
    let base = url::Url::parse(api)?;
    anyhow::ensure!(
        u.scheme() == base.scheme()
            && u.host_str() == base.host_str()
            && u.port_or_known_default() == base.port_or_known_default()
            && u.username().is_empty()
            && u.password().is_none()
            && u.path()
                .starts_with(&format!("{}/", base.path().trim_end_matches('/'))),
        "Invalid Microsoft pagination URL"
    );
    Ok(())
}
pub fn sync(p: &Profile, cancel: &AtomicBool) -> Result<()> {
    let client = client()?;
    let access = token(p, &client)?;
    sync_token(p, &client, &access, cancel)
}
fn sync_token(p: &Profile, client: &Client, access: &str, cancel: &AtomicBool) -> Result<()> {
    sync_with_api(p, client, access, cancel, API, &mut Store::open()?)
}
fn sync_with_api(
    p: &Profile,
    client: &Client,
    access: &str,
    cancel: &AtomicBool,
    api: &str,
    store: &mut Store,
) -> Result<()> {
    store.sync_progress(&p.email, "scanning_folders", 0, None)?;
    let mut folders = std::collections::HashMap::new();
    for (wellknown, label) in [
        ("inbox", "Inbox"),
        ("sentitems", "Sent"),
        ("deleteditems", "Trash"),
        ("junkemail", "Spam"),
        ("drafts", "Server Drafts"),
        ("archive", "Archive"),
    ] {
        cancelled(cancel)?;
        let response = client
            .get(format!("{api}/me/mailFolders/{wellknown}?$select=id"))
            .header("Prefer", "IdType=\"ImmutableId\"")
            .bearer_auth(access)
            .send()?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        let value: Value = checked(response)?.json()?;
        folders.insert(
            value["id"]
                .as_str()
                .context("Missing folder ID")?
                .to_string(),
            label.to_string(),
        );
    }
    // Discover custom and empty folders, walking children and every page.
    let mut pending = vec![(format!("{api}/me/mailFolders?includeHiddenFolders=true&$top=100&$select=id,displayName,childFolderCount"), String::new())];
    let mut folder_pages = std::collections::HashSet::new();
    while let Some((page, parent)) = pending.pop() {
        cancelled(cancel)?;
        trusted_page(&page, api)?;
        anyhow::ensure!(
            folder_pages.insert(page.clone()),
            "Repeated Microsoft folder page"
        );
        let value: Value = checked(
            client
                .get(&page)
                .header("Prefer", "IdType=\"ImmutableId\"")
                .bearer_auth(access)
                .send()?,
        )?
        .json()?;
        for item in value["value"]
            .as_array()
            .context("Missing Microsoft folders")?
        {
            let id = item["id"].as_str().context("Missing folder ID")?;
            let name = item["displayName"]
                .as_str()
                .context("Missing folder name")?;
            let path = if parent.is_empty() {
                name.to_string()
            } else {
                format!("{parent}/{name}")
            };
            let path = folders.entry(id.to_string()).or_insert(path).clone();
            if item["childFolderCount"].as_u64().unwrap_or(0) > 0 {
                let mut url = url::Url::parse(&format!("{api}/me/mailFolders/"))?;
                url.path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("Invalid Microsoft API URL"))?
                    .pop_if_empty()
                    .push(id)
                    .push("childFolders");
                url.query_pairs_mut()
                    .append_pair("includeHiddenFolders", "true")
                    .append_pair("$top", "100")
                    .append_pair("$select", "id,displayName,childFolderCount");
                pending.push((url.into(), path));
            }
        }
        if let Some(next) = value["@odata.nextLink"].as_str() {
            pending.push((next.to_string(), parent));
        }
    }
    // Folder names can be shown before the message import finishes.
    store.replace_account_folders(&p.email, &folders.values().cloned().collect::<Vec<_>>())?;
    let mut ordered = folders.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(id, path)| (path.as_str() != "Inbox", path.as_str(), id.as_str()));
    let mut completed = 0;
    for (id, path) in ordered {
        sync_folder(
            p,
            client,
            access,
            cancel,
            api,
            store,
            id,
            path,
            &mut completed,
        )?;
    }
    store.graph_finish_account(&p.email, &folders.into_keys().collect::<Vec<_>>())?;
    store.prune_sync_history(&p.email)
}

fn graph_delta_url(api: &str, folder_id: &str) -> Result<String> {
    let mut url = url::Url::parse(&format!("{api}/me/mailFolders/"))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Microsoft API URL"))?
        .pop_if_empty()
        .push(folder_id)
        .push("messages")
        .push("delta");
    // Date-filtered delta queries are capped at 5,000 messages. Read lightweight
    // metadata without a filter; fetch bodies only inside the local history range.
    url.query_pairs_mut()
        .append_pair("$orderby", "receivedDateTime desc")
        .append_pair(
            "$select",
            "id,receivedDateTime,parentFolderId,isRead,flag,changeKey",
        );
    Ok(url.into())
}

#[allow(clippy::too_many_arguments)]
fn sync_folder(
    p: &Profile,
    client: &Client,
    access: &str,
    cancel: &AtomicBool,
    api: &str,
    store: &mut Store,
    folder_id: &str,
    path: &str,
    completed: &mut usize,
) -> Result<()> {
    let mut state = store.graph_prepare_folder(&p.email, folder_id, path)?;
    let initial_url = graph_delta_url(api, folder_id)?;
    let mut page = if state.link.is_empty() {
        initial_url.clone()
    } else {
        state.link.clone()
    };
    let mut visited = std::collections::HashSet::new();
    let mut reset = false;
    let cutoff = store.sync_history(&p.email)?.cutoff();
    loop {
        cancelled(cancel)?;
        trusted_page(&page, api)?;
        anyhow::ensure!(visited.insert(page.clone()), "Repeated Microsoft sync page");
        let response = client
            .get(&page)
            .header("Prefer", "IdType=\"ImmutableId\", odata.maxpagesize=100")
            .bearer_auth(access)
            .send()?;
        let status = response.status();
        if status == reqwest::StatusCode::GONE || status == reqwest::StatusCode::BAD_REQUEST {
            let value: Value = response.json()?;
            let code = value["error"]["code"].as_str().unwrap_or_default();
            let expired = status == reqwest::StatusCode::GONE
                || matches!(
                    code,
                    "syncStateNotFound"
                        | "SyncStateNotFound"
                        | "InvalidDeltaToken"
                        | "invalidDeltaToken"
                        | "resyncRequired"
                );
            anyhow::ensure!(
                expired && !reset,
                "Microsoft delta sync failed ({status}): {}",
                microsoft_error_detail(&value)
            );
            store.graph_reset_folder(&p.email, folder_id)?;
            state.initial = true;
            reset = true;
            visited.clear();
            page = initial_url.clone();
            continue;
        }
        let value: Value = checked(response)?.json()?;
        let next = value["@odata.nextLink"].as_str();
        let delta = value["@odata.deltaLink"].as_str();
        anyhow::ensure!(
            next.is_some() != delta.is_some(),
            "Missing or ambiguous Microsoft delta checkpoint"
        );
        let link = next.or(delta).unwrap();
        trusted_page(link, api)?;
        if next.is_some() {
            anyhow::ensure!(!visited.contains(link), "Repeated Microsoft sync page");
        }
        let mut changes = Vec::new();
        for item in value["value"]
            .as_array()
            .context("Missing Microsoft messages")?
        {
            cancelled(cancel)?;
            let id = item["id"]
                .as_str()
                .context("Missing Microsoft message ID")?;
            let change_key = item["changeKey"].as_str().unwrap_or_default();
            if item.get("@removed").is_some() {
                changes.push((id.to_string(), None, String::new()));
                continue;
            }
            let cached = store.cached_message_optional(&p.email, id)?;
            let received = item["receivedDateTime"]
                .as_str()
                .map(chrono::DateTime::parse_from_rfc3339)
                .transpose()?
                .map(|d| d.timestamp_millis())
                .or_else(|| cached.as_ref().map(|m| m.timestamp));
            if cutoff
                .zip(received)
                .is_some_and(|(since, received)| received < since)
            {
                continue;
            }
            let unchanged = !change_key.is_empty()
                && store.graph_change_key(&p.email, id)?.as_deref() == Some(change_key);
            let mut message = if let Some(cached) = cached.as_ref().filter(|_| unchanged) {
                cached.clone()
            } else {
                let mut url = url::Url::parse(&format!("{api}/me/messages/"))?;
                url.path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("Invalid Microsoft API URL"))?
                    .pop_if_empty()
                    .push(id);
                url.query_pairs_mut().append_pair("$select", "id,from,subject,body,receivedDateTime,parentFolderId,isRead,flag,toRecipients,ccRecipients,replyTo,internetMessageId,conversationId,changeKey");
                let response = client
                    .get(url)
                    .header("Prefer", "IdType=\"ImmutableId\"")
                    .bearer_auth(access)
                    .send()?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    changes.push((id.to_string(), None, String::new()));
                    continue;
                }
                let full: Value = checked(response)?.json()?;
                anyhow::ensure!(
                    full["id"].as_str() == Some(id),
                    "Microsoft returned a different message ID"
                );
                // A message can move between the delta event and its content request.
                if full["parentFolderId"]
                    .as_str()
                    .is_some_and(|parent| parent != folder_id)
                {
                    changes.push((id.to_string(), None, String::new()));
                    continue;
                }
                let mut message = summary(&full, &p.email, path)?;
                if cutoff.is_some_and(|since| message.timestamp < since) {
                    continue;
                }
                if path != "Server Drafts" {
                    if let Some(old) = cached
                        .as_ref()
                        .filter(|m| m.attachments_loaded && m.body == message.body)
                    {
                        message.update_content(old);
                    }
                }
                message
            };
            message.folder = path.to_string();
            // Prefer the full GET's flags if it was needed; delta metadata can be older.
            if unchanged {
                if let Some(read) = item["isRead"].as_bool() {
                    message.unread = !read;
                }
                if item.get("flag").is_some() {
                    message.flagged = item["flag"]["flagStatus"] == "flagged";
                }
            }
            changes.push((id.to_string(), Some(message), change_key.to_string()));
        }
        cancelled(cancel)?;
        store.graph_apply_page(&p.email, folder_id, &changes, link, delta.is_some())?;
        *completed += changes.len();
        store.sync_progress(
            &p.email,
            if state.initial {
                "downloading"
            } else {
                "updating"
            },
            *completed,
            None,
        )?;
        store.prune_sync_history(&p.email)?;
        if delta.is_some() {
            return Ok(());
        }
        page = link.to_string();
    }
}

fn addresses(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["emailAddress"]["address"].as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
fn summary(v: &Value, account: &str, folder: &str) -> Result<Message> {
    let html = if v["body"]["contentType"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("html"))
    {
        Some(crate::html::parse(
            v["body"]["content"].as_str().unwrap_or_default(),
        ))
    } else {
        None
    };
    let body = html
        .as_ref()
        .map(|h| h.plain_text())
        .unwrap_or_else(|| v["body"]["content"].as_str().unwrap_or_default().into());
    let date = v["receivedDateTime"].as_str().unwrap_or_default();
    Ok(Message {
        id: v["id"]
            .as_str()
            .context("Missing Microsoft message ID")?
            .into(),
        account: account.into(),
        sender: v["from"]["emailAddress"]["name"]
            .as_str()
            .unwrap_or_default()
            .into(),
        email: v["from"]["emailAddress"]["address"]
            .as_str()
            .unwrap_or_default()
            .into(),
        subject: v["subject"].as_str().unwrap_or_default().into(),
        body,
        date: date.into(),
        timestamp: chrono::DateTime::parse_from_rfc3339(date)
            .map(|d| d.timestamp_millis())
            .unwrap_or_default(),
        folder: folder.into(),
        unread: !v["isRead"].as_bool().unwrap_or(true),
        flagged: v["flag"]["flagStatus"] == "flagged",
        labels: vec![],
        attachments: vec![],
        attachments_loaded: false,
        html,
        html_loaded: true,
        to_header: addresses(&v["toRecipients"]),
        cc_header: addresses(&v["ccRecipients"]),
        reply_to: addresses(&v["replyTo"]),
        delivered_to: account.into(),
        message_id: v["internetMessageId"].as_str().unwrap_or_default().into(),
        references: String::new(),
        thread_id: String::new(),
        recipients_loaded: true,
    })
}
pub fn load_content(p: &Profile, m: &Message) -> Result<Message> {
    let client = client()?;
    let access = token(p, &client)?;
    let mut u = message_url(&m.id)?;
    u.path_segments_mut().unwrap().push("$value");
    let response = checked(
        client
            .get(u)
            .header("Prefer", "IdType=\"ImmutableId\"")
            .bearer_auth(access)
            .send()?,
    )?;
    use std::io::Read;
    let mut bytes = Vec::new();
    response
        .take(40 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let mut loaded = crate::mime::parse(&bytes, &m.account, &m.id, &m.folder)?;
    loaded.unread = m.unread;
    loaded.flagged = m.flagged;
    Ok(loaded)
}
pub struct MutationSession {
    client: Client,
    access: String,
}
impl MutationSession {
    pub fn open(p: &Profile) -> Result<Self> {
        let client = client()?;
        let access = token(p, &client)?;
        Ok(Self { client, access })
    }
    pub fn apply(&mut self, m: &Message, action: Action) -> Result<()> {
        let mut u = message_url(&m.id)?;
        let request = match action {
            Action::Read(read) => self.client.patch(u).json(&json!({"isRead":read})),
            Action::Star(star) => self
                .client
                .patch(u)
                .json(&json!({"flag":{"flagStatus":if star{"flagged"}else{"notFlagged"}}})),
            Action::Archive | Action::Trash => {
                u.path_segments_mut().unwrap().push("move");
                self.client.post(u).json(&json!({"destinationId":if matches!(action,Action::Trash){"deleteditems"}else{"archive"}}))
            }
        };
        checked(
            request
                .header("Prefer", "IdType=\"ImmutableId\"")
                .bearer_auth(&self.access)
                .send()?,
        )?;
        Ok(())
    }
}
pub fn forget_credentials(email: &str) -> Result<()> {
    match entry(email)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn modify(p: &Profile, m: &Message, action: Action, cancel: &AtomicBool) -> Result<String> {
    cancelled(cancel)?;
    let mut session = MutationSession::open(p)?;
    session.apply(m, action)?;
    match sync_token(p, &session.client, &session.access, cancel) {
        Ok(()) => Ok("Mail updated".into()),
        Err(e) => Ok(format!("Mail updated; refresh needed: {e}")),
    }
}
pub fn send(p: &Profile, raw: &[u8], cancel: &AtomicBool) -> Result<()> {
    use base64::Engine;
    let client = client()?;
    let access = token(p, &client)?;
    cancelled(cancel)?;
    let response = client
        .post(endpoint("/me/sendMail"))
        .bearer_auth(access)
        .header("Content-Type", "text/plain")
        .body(base64::engine::general_purpose::STANDARD.encode(raw))
        .send()
        .context("Send outcome unknown; check Sent before retrying")?;
    anyhow::ensure!(
        response.status().is_success(),
        "Microsoft send returned {}; check Sent before retrying",
        response.status()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };
    fn mock(pages: Vec<(u16, Value)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}/v1.0", listener.local_addr().unwrap());
        let mock_api = api.clone();
        let worker = std::thread::spawn(move || {
            for (status, mut body) in pages {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    request.push_str(&line);
                }
                if let Some(expected) = body["_expect"].as_str() {
                    assert!(
                        request.lines().next().unwrap().contains(expected),
                        "Expected {expected}, got {request}"
                    );
                }
                if request.lines().next().unwrap().contains("/messages/delta?") {
                    assert!(request.to_lowercase().contains("odata.maxpagesize=100"));
                    let target = request.split_whitespace().nth(1).unwrap();
                    let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
                    assert!(!url.query_pairs().any(|(key, _)| key == "$top"));
                }
                assert!(request.contains("Bearer test-token"));
                assert!(request.to_lowercase().contains("immutableid"));
                body.as_object_mut().unwrap().remove("_expect");
                let bytes = serde_json::to_string(&body)
                    .unwrap()
                    .replace("__API__", &mock_api)
                    .into_bytes();
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        (api, worker)
    }
    fn message(id: &str) -> Value {
        json!({"id":id,"from":{"emailAddress":{"name":"Alice","address":"alice@example.com"}},"subject":"Test","body":{"contentType":"html","content":"<p>Hello</p><script>bad()</script>"},"receivedDateTime":"2026-11-06T12:00:00Z","parentFolderId":"inbox","isRead":false,"flag":{"flagStatus":"flagged"},"toRecipients":[{"emailAddress":{"address":"me@example.com"}}],"internetMessageId":"<original@example.com>"})
    }
    #[test]
    fn delta_uses_page_size_header_instead_of_a_total_result_limit() {
        let url = url::Url::parse(
            &graph_delta_url("https://graph.microsoft.com/v1.0", "folder/id").unwrap(),
        )
        .unwrap();
        assert!(url.path().contains("folder%2Fid/messages/delta"));
        assert!(!url
            .query_pairs()
            .any(|(key, _)| key == "$top" || key == "$filter"));
    }
    #[test]
    fn graph_pagination_cannot_redirect_credentials_to_another_origin() {
        assert!(trusted_page(
            "https://graph.microsoft.com/v1.0/me/messages?$skip=100",
            API
        )
        .is_ok());
        for url in [
            "https://evil.example/v1.0/me/messages",
            "http://graph.microsoft.com/v1.0/me/messages",
            "https://graph.microsoft.com:444/v1.0/me/messages",
            "https://user@graph.microsoft.com/v1.0/me/messages",
            "https://graph.microsoft.com/beta/me/messages",
        ] {
            assert!(trusted_page(url, API).is_err(), "{url}");
        }
    }
    #[test]
    fn browser_login_uses_pkce_and_loopback_without_device_authentication() {
        let p = Profile {
            email: "me@example.com".into(),
            client_id: "test-client".into(),
            ..Default::default()
        };
        assert!(p.microsoft_sign_in == crate::settings::MicrosoftSignIn::Browser);
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let url =
            browser_authorization_url(&p, "http://localhost:54321/", "random-state", verifier)
                .unwrap();
        assert_eq!(url.host_str(), Some("login.microsoftonline.com"));
        assert_eq!(url.path(), "/common/oauth2/v2.0/authorize");
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["redirect_uri"], "http://localhost:54321/");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["code_challenge"],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(query["state"], "random-state");
        assert_eq!(query["scope"], SCOPE);
        assert!(!url.as_str().contains(verifier));
        assert!(!query.contains_key("client_secret"));
        let mut legacy = serde_json::to_value(&p).unwrap();
        legacy.as_object_mut().unwrap().remove("microsoft_sign_in");
        let legacy: Profile = serde_json::from_value(legacy).unwrap();
        assert!(legacy.microsoft_sign_in == crate::settings::MicrosoftSignIn::Browser);
    }
    #[test]
    fn browser_callback_checks_state_and_keeps_microsoft_denial_details() {
        assert!(
            matches!(browser_callback("GET /?code=authorized&state=expected HTTP/1.1","expected").unwrap(),BrowserCallback::Code(code) if code=="authorized")
        );
        assert!(
            matches!(browser_callback("GET /?error=access_denied&error_description=AADSTS530035%3A%20BlockedBySecurityDefaults&state=expected HTTP/1.1","expected").unwrap(),BrowserCallback::Denied(detail) if detail.contains("AADSTS530035"))
        );
        for request in [
            "GET /?code=stolen&state=wrong HTTP/1.1",
            "GET /?code=one&code=two&state=expected HTTP/1.1",
            "GET /?code=one&state=expected&state=wrong HTTP/1.1",
            "GET /?code=one&error=denied&state=expected HTTP/1.1",
            "POST /?code=one&state=expected HTTP/1.1",
            "GET /other?code=one&state=expected HTTP/1.1",
            "GET https://evil.example/?code=one&state=expected HTTP/1.1",
        ] {
            assert!(browser_callback(request, "expected").is_err(), "{request}");
        }
    }

    #[test]
    fn microsoft_errors_keep_resource_denials_and_aadsts_codes_without_credentials() {
        let body = json!({"error":"invalid_resource","error_description":"AADSTS650057: Resource is not listed in the application's permissions.\r\nTrace ID: private-diagnostic","error_codes":[650057],"access_token":"private-access-token","refresh_token":"private-refresh-token","device_code":"private-device-code"});
        let detail = microsoft_error_detail(&body);
        assert!(detail.contains("AADSTS650057"));
        assert!(detail.contains("Resource is not listed"));
        assert!(!detail.contains("private-"));
        let graph = microsoft_error_detail(
            &json!({"error":{"code":"ErrorAccessDenied","message":"Access to OData is disabled."}}),
        );
        assert_eq!(graph, "ErrorAccessDenied: Access to OData is disabled.");
        assert!(
            microsoft_error_detail(&json!({"error_description":"x".repeat(5000)})).len() < 1000
        );
    }

    #[test]
    fn device_prompt_exposes_only_the_user_code_and_errors_explain_missing_codes() {
        let prompt = device_authorization_prompt(&json!({"user_code":"TEST-CODE","device_code":"private-device-secret","expires_in":900})).unwrap();
        let value = serde_json::to_value(&prompt).unwrap();
        assert_eq!(value, json!({"user_code":"TEST-CODE","expires_in":900}));
        assert!(!serde_json::to_string(&prompt)
            .unwrap()
            .contains("private-device-secret"));
        assert!(device_authorization_prompt(&json!({"user_code":""})).is_err());
        let error = validate_device_authorization(
            reqwest::StatusCode::BAD_REQUEST,
            &json!({"error":"unauthorized_client"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("could not issue a sign-in code"));
        assert!(error.contains("public client"));
    }

    #[test]
    fn expired_microsoft_sign_in_is_distinct_from_network_and_application_errors() {
        for error in [
            "invalid_grant",
            "interaction_required",
            "login_required",
            "consent_required",
        ] {
            let e = validate_refresh(reqwest::StatusCode::BAD_REQUEST, &json!({"error":error}))
                .unwrap_err();
            assert!(e.is::<crate::service::ReconnectRequired>());
        }
        assert!(
            validate_refresh(reqwest::StatusCode::UNAUTHORIZED, &json!({}))
                .unwrap_err()
                .is::<crate::service::ReconnectRequired>()
        );
        for (status, body) in [
            (reqwest::StatusCode::BAD_GATEWAY, json!({})),
            (
                reqwest::StatusCode::BAD_REQUEST,
                json!({"error":"invalid_client"}),
            ),
        ] {
            assert!(!validate_refresh(status, &body)
                .unwrap_err()
                .is::<crate::service::ReconnectRequired>());
        }
    }
    #[test]
    fn graph_summary_maps_headers_flags_and_safe_html() {
        let m = summary(&message("id"), "me@example.com", "Inbox").unwrap();
        assert!(m.unread);
        assert!(m.flagged);
        assert!(m.body.contains("Hello"));
        assert!(!m.body.contains("bad()"));
        assert_eq!(m.to_header, "me@example.com");
        assert!(!m.attachments_loaded);
        assert!(m.timestamp > 0);
    }
    #[test]
    fn successful_graph_pagination_replaces_only_its_account_and_keeps_hydrated_content() {
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let p = Profile {
            email: "me@example.com".into(),
            ..Default::default()
        };
        let mut old = summary(&message("first"), &p.email, "Inbox").unwrap();
        old.attachments_loaded = true;
        old.references = "<reference@example.com>".into();
        store.snapshot(&p.email, &[old], "old").unwrap();
        store
            .snapshot(
                "other@example.com",
                &[summary(&message("other"), "other@example.com", "Inbox").unwrap()],
                "other-cursor",
            )
            .unwrap();
        let mut pages = ["inbox", "sent", "trash", "spam", "drafts", "archive"]
            .into_iter()
            .map(|id| (200, json!({"id": id})))
            .collect::<Vec<_>>();
        pages.push((200, json!({"value":[{"id":"projects","displayName":"Projects","childFolderCount":1}],"@odata.nextLink":"__API__/me/mailFolders?$skip=100"})));
        pages.push((
            200,
            json!({"value":[{"id":"empty","displayName":"Empty folder","childFolderCount":0}]}),
        ));
        pages.push((
            200,
            json!({"value":[{"id":"client","displayName":"Client A","childFolderCount":0}]}),
        ));
        pages.push((200, json!({"_expect":"/mailFolders/inbox/messages/delta?","value":[message("first")], "@odata.nextLink":"__API__/me/mailFolders/inbox/messages/delta?page=2"})));
        pages.push((200, message("first")));
        pages.push((200, json!({"_expect":"delta?page=2","value":[],"@odata.deltaLink":"__API__/me/mailFolders/inbox/messages/delta?token=1"})));
        for id in [
            "archive", "empty", "projects", "client", "sent", "drafts", "spam", "trash",
        ] {
            let values = if id == "client" {
                vec![json!({"id":"second","receivedDateTime":"2026-11-06T12:00:00Z"})]
            } else {
                vec![]
            };
            pages.push((200,json!({"_expect":format!("/mailFolders/{id}/messages/delta?"),"value":values,"@odata.deltaLink":format!("__API__/me/mailFolders/{id}/messages/delta?token=1")})));
            if id == "client" {
                let mut custom = message("second");
                custom["parentFolderId"] = json!("client");
                pages.push((200, custom));
            }
        }
        let (api, worker) = mock(pages);
        sync_with_api(
            &p,
            &client().unwrap(),
            "test-token",
            &AtomicBool::new(false),
            &api,
            &mut store,
        )
        .unwrap();
        let messages = store.load().unwrap();
        assert_eq!(messages.len(), 3);
        assert!(
            messages
                .iter()
                .find(|m| m.id == "first")
                .unwrap()
                .attachments_loaded
        );
        assert_eq!(
            messages
                .iter()
                .find(|m| m.id == "first")
                .unwrap()
                .references,
            "<reference@example.com>"
        );
        assert_eq!(
            messages.iter().find(|m| m.id == "second").unwrap().folder,
            "Projects/Client A"
        );
        assert!(store.account_folders().unwrap()[&p.email]
            .as_array()
            .unwrap()
            .contains(&json!("Empty folder")));
        assert_eq!(store.history(&p.email).unwrap(), "graph-delta-v1");
        assert_eq!(store.history("other@example.com").unwrap(), "other-cursor");
        worker.join().unwrap();
    }
    #[test]
    fn failed_or_untrusted_graph_page_preserves_existing_cache_and_cursor() {
        for bad_page in [false, true] {
            let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
            let p = Profile {
                email: "me@example.com".into(),
                ..Default::default()
            };
            store
                .snapshot(
                    &p.email,
                    &[summary(&message("old"), &p.email, "Inbox").unwrap()],
                    "old-cursor",
                )
                .unwrap();
            let mut pages = ["inbox", "sent", "trash", "spam", "drafts", "archive"]
                .into_iter()
                .map(|id| (200, json!({"id": id})))
                .collect::<Vec<_>>();
            pages.push((200, json!({"value":[]})));
            if bad_page {
                pages.push((200,json!({"value":[message("new")],"@odata.nextLink":"https://evil.example/steal"})));
            } else {
                pages.push((503, json!({"error":{"code":"Unavailable"}})));
            }
            let (api, worker) = mock(pages);
            assert!(sync_with_api(
                &p,
                &client().unwrap(),
                "test-token",
                &AtomicBool::new(false),
                &api,
                &mut store
            )
            .is_err());
            assert_eq!(store.load().unwrap()[0].id, "old");
            assert_eq!(store.history(&p.email).unwrap(), "old-cursor");
            worker.join().unwrap();
        }
    }
    #[test]
    fn delta_pages_are_visible_resume_after_failure_and_incremental_runs_keep_unchanged_mail() {
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let p = Profile {
            email: "me@example.com".into(),
            ..Default::default()
        };
        store.snapshot(&p.email, &[], "").unwrap();
        let mut first = message("first");
        first["changeKey"] = json!("v1");
        let mut updated = first.clone();
        updated["isRead"] = json!(true);
        updated["changeKey"] = json!("v2");
        let (api, worker) = mock(vec![
            (
                200,
                json!({"_expect":"/mailFolders/inbox/messages/delta?","value":[first.clone()],"@odata.nextLink":"__API__/me/mailFolders/inbox/messages/delta?page=2"}),
            ),
            (200, first.clone()),
            (
                503,
                json!({"_expect":"delta?page=2","error":{"code":"Unavailable"}}),
            ),
            (
                200,
                json!({"_expect":"delta?page=2","value":[message("second")],"@odata.deltaLink":"__API__/me/mailFolders/inbox/messages/delta?token=1"}),
            ),
            (200, message("second")),
            (
                200,
                json!({"_expect":"delta?token=1","value":[updated.clone(),{"id":"second","@removed":{"reason":"deleted"}}],"@odata.deltaLink":"__API__/me/mailFolders/inbox/messages/delta?token=2"}),
            ),
            (200, updated.clone()),
            (
                200,
                json!({"_expect":"delta?token=2","value":[updated],"@odata.deltaLink":"__API__/me/mailFolders/inbox/messages/delta?token=3"}),
            ),
        ]);
        let client = client().unwrap();
        let cancel = AtomicBool::new(false);
        let mut count = 0;
        assert!(sync_folder(
            &p,
            &client,
            "test-token",
            &cancel,
            &api,
            &mut store,
            "inbox",
            "Inbox",
            &mut count
        )
        .is_err());
        assert_eq!(
            store.mailbox_page("All Inboxes", "", 0).unwrap()["total"],
            1
        );
        assert_eq!(
            store.cached_message(&p.email, "first").unwrap().body,
            "Hello"
        );
        assert!(store
            .graph_prepare_folder(&p.email, "inbox", "Inbox")
            .unwrap()
            .link
            .ends_with("page=2"));
        assert!(store.history(&p.email).unwrap().is_empty());
        sync_folder(
            &p,
            &client,
            "test-token",
            &cancel,
            &api,
            &mut store,
            "inbox",
            "Inbox",
            &mut count,
        )
        .unwrap();
        assert_eq!(store.cached_ids(&p.email).unwrap().len(), 2);
        let first_cached = store.cached_message(&p.email, "first").unwrap();
        sync_folder(
            &p,
            &client,
            "test-token",
            &cancel,
            &api,
            &mut store,
            "inbox",
            "Inbox",
            &mut count,
        )
        .unwrap();
        assert!(!store.cached_message(&p.email, "first").unwrap().unread);
        assert!(store
            .cached_message_optional(&p.email, "second")
            .unwrap()
            .is_none());
        sync_folder(
            &p,
            &client,
            "test-token",
            &cancel,
            &api,
            &mut store,
            "inbox",
            "Inbox",
            &mut count,
        )
        .unwrap();
        assert_eq!(
            store.cached_message(&p.email, "first").unwrap().body,
            first_cached.body
        );
        worker.join().unwrap();
    }
    #[test]
    fn delta_history_skips_old_bodies_and_expired_cursor_restarts_only_its_folder() {
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let p = Profile {
            email: "me@example.com".into(),
            ..Default::default()
        };
        let old = summary(&message("stale"), &p.email, "Inbox").unwrap();
        store.snapshot(&p.email, &[old], "previous").unwrap();
        let (api, worker) = mock(vec![
            (
                410,
                json!({"_expect":"delta?expired","error":{"code":"syncStateNotFound"}}),
            ),
            (
                200,
                json!({"_expect":"/mailFolders/inbox/messages/delta?","value":[{"id":"old","receivedDateTime":"2000-01-01T00:00:00Z"},message("new")],"@odata.deltaLink":"__API__/me/mailFolders/inbox/messages/delta?token=new"}),
            ),
            (200, message("new")),
        ]);
        store
            .graph_prepare_folder(&p.email, "inbox", "Inbox")
            .unwrap();
        store
            .graph_apply_page(
                &p.email,
                "inbox",
                &[],
                &format!("{api}/me/mailFolders/inbox/messages/delta?expired"),
                false,
            )
            .unwrap();
        sync_folder(
            &p,
            &client().unwrap(),
            "test-token",
            &AtomicBool::new(false),
            &api,
            &mut store,
            "inbox",
            "Inbox",
            &mut 0,
        )
        .unwrap();
        assert!(store
            .cached_message_optional(&p.email, "old")
            .unwrap()
            .is_none());
        assert!(store
            .cached_message_optional(&p.email, "stale")
            .unwrap()
            .is_none());
        assert!(store
            .cached_message_optional(&p.email, "new")
            .unwrap()
            .is_some());
        worker.join().unwrap();
    }
}
