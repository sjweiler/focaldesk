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
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
const API: &str = "https://graph.microsoft.com/v1.0";
const SCOPE: &str = "offline_access User.Read Mail.ReadWrite Mail.Send";
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
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(crate::service::ReconnectRequired.into());
    }
    anyhow::ensure!(
        status.is_success(),
        "Microsoft request failed ({status}); check permissions or retry later"
    );
    Ok(response)
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
    let device: Value = checked(
        client
            .post(format!("{authority}/devicecode"))
            .form(&[("client_id", p.client_id.as_str()), ("scope", SCOPE)])
            .send()?,
    )?
    .json()?;
    let code = device["device_code"]
        .as_str()
        .context("Missing device code")?;
    let user_code = device["user_code"]
        .as_str()
        .context("Missing sign-in code")?;
    progress(&format!(
        "Open https://microsoft.com/devicelogin and enter {user_code}"
    ));
    let _ = webbrowser::open("https://microsoft.com/devicelogin");
    let deadline =
        Instant::now() + Duration::from_secs(device["expires_in"].as_u64().unwrap_or(900).min(900));
    let mut interval = device["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let token = loop {
        wait(&cancel, interval)?;
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
            Some("slow_down") => interval += 5,
            _ => bail!(
                "Microsoft sign-in was declined, expired, or blocked by application/tenant policy"
            ),
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
    progress("Microsoft account authorized; synchronizing mailbox…");
    let result = sync_token(p, &client, access, &cancel);
    Store::open()?.record_status(&p.email, &result)?;
    result?;
    Ok(format!("Connected {}", p.email))
}
fn token(p: &Profile, client: &Client) -> Result<String> {
    let saved: Credential = serde_json::from_str(
        &entry(&p.email)?
            .get_password()
            .context("Unlock keyring or reconnect Microsoft account")?,
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
    if body["error"] == "invalid_grant" || body["error"] == "interaction_required" {
        return Err(crate::service::ReconnectRequired.into());
    }
    anyhow::ensure!(
        status.is_success(),
        "Microsoft token refresh failed ({status})"
    );
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
    let mut folders = std::collections::HashMap::new();
    for (wellknown, label) in [
        ("inbox", "Inbox"),
        ("sentitems", "Sent"),
        ("deleteditems", "Trash"),
        ("junkemail", "Spam"),
        ("drafts", "Server Drafts"),
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
            label,
        );
    }
    let mut next=Some(format!("{api}/me/messages?$top=100&$select=id,from,subject,body,receivedDateTime,parentFolderId,isRead,flag,toRecipients,ccRecipients,replyTo,internetMessageId,conversationId"));
    let cached = store
        .load()?
        .into_iter()
        .filter(|m| m.account == p.email)
        .map(|m| (m.id.clone(), m))
        .collect::<std::collections::HashMap<_, _>>();
    let mut messages = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(page) = next {
        cancelled(cancel)?;
        trusted_page(&page, api)?;
        anyhow::ensure!(visited.insert(page.clone()), "Repeated Microsoft sync page");
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
            .context("Missing Microsoft messages")?
        {
            let folder = folders
                .get(item["parentFolderId"].as_str().unwrap_or_default())
                .copied()
                .unwrap_or("Archive");
            let mut message = summary(item, &p.email, folder)?;
            if folder != "Server Drafts" {
                if let Some(old) = cached
                    .get(&message.id)
                    .filter(|m| m.attachments_loaded && m.body == message.body)
                {
                    message.update_content(old);
                }
            }
            messages.push(message);
        }
        next = value["@odata.nextLink"].as_str().map(String::from);
    }
    cancelled(cancel)?;
    store.snapshot(&p.email, &messages, "graph-full-v1")
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
pub fn modify(p: &Profile, m: &Message, action: Action, cancel: &AtomicBool) -> Result<String> {
    let client = client()?;
    let access = token(p, &client)?;
    cancelled(cancel)?;
    let mut u = message_url(&m.id)?;
    let request = match action {
        Action::Read(read) => client.patch(u).json(&json!({"isRead":read})),
        Action::Star(star) => client
            .patch(u)
            .json(&json!({"flag":{"flagStatus":if star{"flagged"}else{"notFlagged"}}})),
        Action::Archive | Action::Trash => {
            u.path_segments_mut().unwrap().push("move");
            client.post(u).json(&json!({"destinationId":if matches!(action,Action::Trash){"deleteditems"}else{"archive"}}))
        }
    };
    checked(
        request
            .header("Prefer", "IdType=\"ImmutableId\"")
            .bearer_auth(&access)
            .send()?,
    )?;
    match sync_token(p, &client, &access, cancel) {
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
            for (status, body) in pages {
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
                assert!(request.contains("Bearer test-token"));
                assert!(request.to_lowercase().contains("immutableid"));
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
        let mut pages = ["inbox", "sent", "trash", "spam", "drafts"]
            .into_iter()
            .map(|id| (200, json!({"id": id})))
            .collect::<Vec<_>>();
        pages.push((
            200,
            json!({"value":[message("first")], "@odata.nextLink":"__API__/me/messages?$skip=100"}),
        ));
        pages.push((200, json!({"value":[message("second")]})));
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
        assert_eq!(store.history(&p.email).unwrap(), "graph-full-v1");
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
            let mut pages = ["inbox", "sent", "trash", "spam", "drafts"]
                .into_iter()
                .map(|id| (200, json!({"id": id})))
                .collect::<Vec<_>>();
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
}
