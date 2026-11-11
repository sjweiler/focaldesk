//! The webview handles presentation only. Mail and credentials stay in Rust.
use crate::{attachments, drafts, service, settings, store, Message};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::Emitter;

#[derive(Default)]
struct State {
    busy: Mutex<()>,
    cancel: Arc<AtomicBool>,
    device_authorization: Mutex<Option<crate::outlook::DeviceAuthorization>>,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Snapshot,
    Signature {
        account: String,
    },
    SetSignature {
        account: String,
        signature: crate::compose::Signature,
    },
    SyncStatus,
    AccountAuthorization,
    PauseSync,
    SetSyncHistory {
        account: String,
        history: settings::SyncHistory,
    },
    RemoteImages {
        account: String,
        id: String,
    },
    Mailbox {
        folder: String,
        query: String,
        offset: usize,
    },
    CachedMessage {
        account: String,
        id: String,
    },
    AccountProfile {
        email: String,
    },
    Sync,
    Connect {
        path: String,
    },
    ConnectProfile {
        profile: settings::Profile,
        password: String,
    },
    Reconnect {
        email: String,
    },
    Notifications {
        enabled: bool,
    },
    Content {
        account: String,
        id: String,
    },
    BulkModify {
        targets: Vec<crate::bulk::Target>,
        action: service::client::Action,
    },
    RemoveAccount {
        account: String,
    },
    Modify {
        account: String,
        id: String,
        action: service::client::Action,
    },
    Download {
        account: String,
        id: String,
        index: usize,
    },
    NewDraft {
        account: String,
        id: String,
        kind: String,
    },
    LoadDraft {
        id: String,
    },
    SaveDraft {
        draft: drafts::Draft,
    },
    DeleteDraft {
        id: String,
    },
    Attach {
        id: String,
        name: String,
        data: Vec<u8>,
    },
    RemoveAttachment {
        id: String,
        index: usize,
    },
    ForwardAttachments {
        id: String,
        selected: Vec<usize>,
    },
    Send {
        id: String,
    },
    OpenLink {
        url: String,
    },
}
fn message(account: &str, id: &str) -> Result<Message> {
    store::Store::open()?.cached_message(account, id)
}
fn draft_value(id: &str) -> Result<Value> {
    let (draft, files) = store::Store::open()?.load_local_draft(id)?;
    Ok(
        json!({"draft": draft, "files": files.iter().map(|f| json!({"name":f.name,"size":f.data.len()})).collect::<Vec<_>>()}),
    )
}
fn sync_status() -> Result<Value> {
    let mut status = store::Store::open()?.account_status()?;
    let available = service::available();
    if let Some(accounts) = status.as_array_mut() {
        for account in accounts {
            account["service_available"] = available.into();
        }
    }
    Ok(status)
}
fn execute(request: Request, state: &State, app: &tauri::AppHandle) -> Result<Value> {
    use service::client as mail;
    let cancel = state.cancel.clone();
    let progress = |text: &str| {
        if let Some(payload) = text.strip_prefix(crate::outlook::DEVICE_AUTHORIZATION_PREFIX) {
            if let Ok(prompt) = serde_json::from_str::<crate::outlook::DeviceAuthorization>(payload)
            {
                *state.device_authorization.lock().unwrap() = Some(prompt.clone());
                let _ = app.emit("mail-device-authorization", prompt);
            }
        } else {
            if text.starts_with("Microsoft account authorized")
                || text.starts_with("Microsoft browser response received")
            {
                *state.device_authorization.lock().unwrap() = None;
            }
            let _ = app.emit("mail-progress", text);
        }
    };
    Ok(match request {
        Request::BulkModify { targets, action } => {
            json!(mail::bulk_modify(targets, action, cancel, progress)?)
        }
        Request::RemoveAccount { account } => json!(mail::remove_account(account, cancel)?),
        Request::RemoteImages { account, id } => {
            json!(crate::remote_images::load(&message(&account, &id)?)?)
        }
        Request::SyncStatus => sync_status()?,
        Request::AccountAuthorization => {
            serde_json::to_value(&*state.device_authorization.lock().unwrap())?
        }
        Request::PauseSync => json!(mail::pause_background()?),
        Request::SetSyncHistory { account, history } => {
            json!(mail::set_sync_history(account, history, cancel)?)
        }
        Request::Snapshot => {
            let s = store::Store::open()?;
            json!({"accounts":s.accounts()?, "folders":s.account_folders()?, "drafts":s.list_drafts()?, "active":s.active_draft_id()?, "profiles":s.profiles()?, "status":sync_status()?, "notifications":s.notifications_enabled()?})
        }
        Request::Mailbox {
            folder,
            query,
            offset,
        } => store::Store::open()?.mailbox_page(&folder, &query, offset)?,
        Request::CachedMessage { account, id } => {
            let s = store::Store::open()?;
            let mut value = serde_json::to_value(s.cached_message(&account, &id)?)?;
            value["pending_move"] = s.move_pending(&account, &id)?.into();
            value
        }
        Request::AccountProfile { email } => {
            json!(store::Store::open()?.connected_profile(&email)?)
        }
        Request::Sync => {
            service::ensure_running()?;
            json!(mail::sync_all(cancel)?)
        }
        Request::Connect { path } => json!(mail::connect(&path, cancel, progress)?),
        Request::ConnectProfile { profile, password } => {
            profile.validate()?;
            json!(mail::connect_profile(profile, password, cancel, progress)?)
        }
        Request::Reconnect { email } => json!(mail::reconnect(&email, cancel, progress)?),
        Request::Notifications { enabled } => {
            store::Store::open()?.set_notifications(enabled)?;
            Value::Null
        }
        Request::Content { account, id } => json!(mail::load_content(&message(&account, &id)?)?),
        Request::Modify {
            account,
            id,
            action,
        } => json!(mail::modify(&message(&account, &id)?, action, cancel)?),
        Request::Download { account, id, index } => {
            let m = message(&account, &id)?;
            json!(mail::download(
                &m,
                m.attachments.get(index).context("Attachment is missing")?
            )?)
        }
        Request::NewDraft { account, id, kind } => {
            let s = store::Store::open()?;
            let draft = if kind == "new" {
                drafts::Draft::new(s.accounts()?.first().cloned().unwrap_or_default())
            } else {
                let m = mail::load_content(&message(&account, &id)?)?;
                let kind = match kind.as_str() {
                    "reply" => drafts::Kind::Reply,
                    "reply_all" => drafts::Kind::ReplyAll,
                    "forward" => drafts::Kind::Forward,
                    _ => anyhow::bail!("Unknown compose action"),
                };
                drafts::prepare(&m, kind, &s.accounts()?)
            };
            store::Store::open()?.save_local_draft(&draft, Some(&[]))?;
            draft_value(&draft.id)?
        }
        Request::Signature { account } => {
            serde_json::to_value(store::Store::open()?.signature(&account)?)?
        }
        Request::SetSignature { account, signature } => {
            store::Store::open()?.set_signature(&account, &signature)?;
            Value::Null
        }
        Request::LoadDraft { id } => draft_value(&id)?,
        Request::SaveDraft { draft } => {
            anyhow::ensure!(
                draft.body.len() <= 1024 * 1024,
                "Message text may be at most 1 MiB"
            );
            store::Store::open()?.save_local_draft(&draft, None)?;
            Value::Null
        }
        Request::DeleteDraft { id } => {
            store::Store::open()?.delete_local_draft(&id)?;
            Value::Null
        }
        Request::Attach { id, name, data } => {
            let (d, mut files) = store::Store::open()?.load_local_draft(&id)?;
            files.push(attachments::DraftAttachment {
                name: attachments::filename(&name),
                data,
            });
            store::Store::open()?.save_local_draft(&d, Some(&files))?;
            draft_value(&id)?
        }
        Request::RemoveAttachment { id, index } => {
            let (d, mut files) = store::Store::open()?.load_local_draft(&id)?;
            anyhow::ensure!(index < files.len(), "Attachment is missing");
            files.remove(index);
            store::Store::open()?.save_local_draft(&d, Some(&files))?;
            draft_value(&id)?
        }
        Request::ForwardAttachments { id, selected } => {
            let (d, mut files) = store::Store::open()?.load_local_draft(&id)?;
            let remaining =
                attachments::MAX_BYTES.saturating_sub(files.iter().map(|f| f.data.len()).sum());
            files.extend(mail::forward_files(
                d.forward.as_ref().context("No forwarded message")?,
                &selected,
                remaining,
            )?);
            store::Store::open()?.save_local_draft(&d, Some(&files))?;
            draft_value(&id)?
        }
        Request::Send { id } => {
            let (draft, files) = store::Store::open()?.load_local_draft(&id)?;
            json!(mail::send(&draft, &files, cancel)?)
        }
        Request::OpenLink { url } => {
            let url = crate::html::safe_link(&url)
                .context("Only HTTP/HTTPS links without credentials are allowed")?;
            webbrowser::open(&url)?;
            Value::Null
        }
    })
}
#[tauri::command]
async fn mail_request(
    request: Request,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<State>>,
) -> std::result::Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Cached reads must remain available while a provider scan holds the worker.
        if matches!(
            &request,
            Request::Snapshot
                | Request::SyncStatus
                | Request::AccountAuthorization
                | Request::OpenLink { .. }
                | Request::PauseSync
                | Request::RemoteImages { .. }
                | Request::Mailbox { .. }
                | Request::CachedMessage { .. }
                | Request::AccountProfile { .. }
                | Request::Signature { .. }
                | Request::SetSignature { .. }
                | Request::LoadDraft { .. }
        ) {
            return execute(request, &state, &app).map_err(|e| format!("{e:#}"));
        }
        let _guard = state
            .busy
            .lock()
            .map_err(|_| "Mail worker unavailable".to_string())?;
        state.cancel.store(false, Ordering::Relaxed);
        *state.device_authorization.lock().unwrap() = None;
        let result = execute(request, &state, &app).map_err(|e| format!("{e:#}"));
        *state.device_authorization.lock().unwrap() = None;
        result
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn cancel_operation(state: tauri::State<'_, Arc<State>>) {
    state.cancel.store(true, Ordering::Relaxed);
}
pub fn run() -> Result<()> {
    if let Err(e) = service::ensure_running() {
        eprintln!("Mail service unavailable: {e:#}");
    }
    tauri::Builder::default()
        .manage(Arc::new(State::default()))
        .invoke_handler(tauri::generate_handler![mail_request, cancel_operation])
        .run(tauri::generate_context!())
        .context("Could not run FocalMail")
}
