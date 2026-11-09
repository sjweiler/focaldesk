//! Per-user mail service. One JSON request per Unix connection, newline framed.
//! The socket is private to the login user; provider credentials never cross it.
use crate::{
    attachments::{Attachment, DraftAttachment},
    drafts::{Draft, ForwardSource},
    providers as backend,
    store::Store,
    Message,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

const MAX_FRAME: u64 = 96 * 1024 * 1024;
#[derive(Debug)]
pub struct ReconnectRequired;
impl std::fmt::Display for ReconnectRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "Authorization expired or revoked. Reconnect this account in Settings → Accounts.",
        )
    }
}
impl std::error::Error for ReconnectRequired {}

pub fn credential_error(error: keyring::Error) -> anyhow::Error {
    match error {
        keyring::Error::NoEntry => ReconnectRequired.into(),
        error => anyhow::Error::new(error).context("Unlock the desktop keyring and retry"),
    }
}
pub fn is_network_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout())
            || cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(network_io_error)
            || cause
                .downcast_ref::<imap::error::Error>()
                .is_some_and(|e| match e {
                    imap::error::Error::ConnectionLost => true,
                    imap::error::Error::Io(error) => network_io_error(error),
                    _ => false,
                })
    })
}

fn network_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::AddrNotAvailable
    )
}

#[derive(Debug)]
pub struct OperationCancelled;
impl std::fmt::Display for OperationCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Operation cancelled")
    }
}
impl std::error::Error for OperationCancelled {}

#[derive(Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status {},
    Changes {
        after: u64,
        wait_seconds: u64,
    },
    Search {
        query: String,
        account: Option<String>,
        limit: usize,
    },
    Read {
        account: String,
        id: String,
    },
    Sync {},
    PauseSync {},
    SetSyncHistory {
        account: String,
        history: crate::settings::SyncHistory,
    },
    Connect {
        path: String,
    },
    ConnectProfile {
        profile: crate::settings::Profile,
        password: String,
    },
    Reconnect {
        email: String,
    },
    BulkModify {
        targets: Vec<crate::bulk::Target>,
        action: backend::Action,
    },
    RemoveAccount {
        account: String,
    },
    Modify {
        account: String,
        id: String,
        action: backend::Action,
    },
    Send {
        draft: Box<Draft>,
        files: Vec<DraftAttachment>,
    },
    Content {
        account: String,
        id: String,
    },
    Download {
        account: String,
        id: String,
        attachment: Attachment,
    },
    Forward {
        source: ForwardSource,
        selected: Vec<usize>,
        remaining: usize,
    },
    Cancel {
        operation: String,
    },
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    id: String,
    request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Response {
    Progress { detail: String },
    Done { value: Value },
    Error { detail: String },
}
struct State {
    active: Mutex<Option<(String, Arc<AtomicBool>)>>,
    operation: Mutex<()>,
    sync_schedule: Mutex<crate::sync_recovery::Schedule>,
    revision: Mutex<u64>,
    changed: Condvar,
    instance: String,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            operation: Mutex::new(()),
            sync_schedule: Mutex::new(crate::sync_recovery::Schedule::new(Instant::now())),
            revision: Mutex::new(0),
            changed: Condvar::new(),
            instance: format!("{:032x}", rand::random::<u128>()),
        }
    }
}

impl State {
    fn recovery_event(&self, event: crate::sync_recovery::Event) {
        use crate::sync_recovery::Event;
        let mut schedule = self.sync_schedule.lock().unwrap();
        match event {
            Event::Sleep(sleeping) => schedule.sleep(sleeping, Instant::now()),
            Event::NetworkRestored => schedule.request(Instant::now(), Duration::from_secs(2)),
            Event::NetworkLost => {}
        }
        // Cancel only background reads, never an interactive send or mutation.
        if matches!(event, Event::Sleep(_) | Event::NetworkLost) {
            if let Some((id, cancel)) = self.active.lock().unwrap().as_ref() {
                if id == "background-sync" {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        drop(schedule);
        self.notify_change();
    }
    fn notify_change(&self) {
        *self.revision.lock().unwrap() += 1;
        self.changed.notify_all();
    }
}

fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is required for the private mail socket")?;
    Ok(PathBuf::from(runtime).join("focalmail/service.sock"))
}
fn frame<T: serde::de::DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let mut bytes = Vec::new();
    reader.take(MAX_FRAME + 1).read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_FRAME && bytes.last() == Some(&b'\n'),
        "Invalid or oversized mail service frame"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_FRAME,
        "Mail service response exceeds frame limit"
    );
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    Ok(())
}
fn cached(account: &str, id: &str) -> Result<Message> {
    Store::open()?.cached_message(account, id)
}
fn remote_cached(account: &str, id: &str) -> Result<Message> {
    let s = Store::open()?;
    anyhow::ensure!(
        s.accounts()?.iter().any(|a| a == account),
        "Account is no longer connected"
    );
    anyhow::ensure!(
        !s.move_pending(account, id)?,
        "This IMAP move is awaiting folder refresh; sync before another remote action"
    );
    s.cached_message(account, id)
}
fn execute(request: Request, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<Value> {
    let text = match request {
        Request::BulkModify { targets, action } => {
            return Ok(serde_json::to_value(crate::bulk::modify(
                targets, action, cancel, progress,
            )?)?)
        }
        Request::RemoveAccount { account } => crate::bulk::remove_account(&account)?,
        Request::SetSyncHistory { account, history } => {
            let mut store = Store::open()?;
            store.set_sync_history(&account, history)?;
            store.set_sync_paused(false)?;
            "Sync history saved; mail on the server is unchanged".into()
        }
        Request::Sync {} => {
            Store::open()?.set_sync_paused(false)?;
            backend::sync_all(cancel)?
        }
        Request::Connect { path } => backend::connect(&path, cancel, progress)?,
        Request::ConnectProfile { profile, password } => {
            backend::connect_profile(&profile, &password, cancel, progress)?
        }
        Request::Reconnect { email } => backend::reconnect(&email, cancel, progress)?,
        Request::Modify {
            account,
            id,
            action,
        } => backend::modify(&remote_cached(&account, &id)?, action, cancel)?,
        Request::Send { draft, files } => backend::send(&draft, &files, cancel)?,
        Request::Content { account, id } => {
            return Ok(serde_json::to_value(backend::load_content(&cached(
                &account, &id,
            )?)?)?)
        }
        Request::Download {
            account,
            id,
            attachment,
        } => {
            let message = remote_cached(&account, &id)?;
            let attachment = message
                .attachments
                .iter()
                .find(|a| a.id == attachment.id && a.name == attachment.name)
                .context("Attachment is not in this message")?;
            backend::download(&message, attachment)?
        }
        Request::Forward {
            source,
            selected,
            remaining,
        } => {
            let message = remote_cached(&source.account, &source.message_id)?;
            let source = ForwardSource {
                attachments: message.attachments,
                ..source
            };
            return Ok(serde_json::to_value(backend::forward_files(
                &source, &selected, remaining,
            )?)?);
        }
        _ => bail!("Invalid operation"),
    };
    Ok(json!(text))
}
fn handle(mut stream: UnixStream, state: Arc<State>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let result = (|| {
        let envelope: Envelope = frame(&mut BufReader::new(stream.try_clone()?))?;
        dispatch(envelope, &mut stream, &state)
    })();
    write_frame(
        &mut stream,
        &match result {
            Ok(value) => Response::Done { value },
            Err(error) => Response::Error {
                detail: error.to_string(),
            },
        },
    )
}
fn pause_background(state: &State, store: &Store) -> Result<Value> {
    store.set_sync_paused(true)?;
    if let Some((operation, cancel)) = state.active.lock().unwrap().as_ref() {
        if operation == "background-sync" {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    state.notify_change();
    Ok(json!(
        "Background sync paused; cached mail remains available"
    ))
}
fn dispatch(envelope: Envelope, stream: &mut UnixStream, state: &State) -> Result<Value> {
    match envelope.request {
        Request::PauseSync {} => pause_background(state, &Store::open()?),
        Request::Status {} => Ok(
            json!({"version":1,"instance":state.instance,"revision":*state.revision.lock().unwrap(),"accounts":Store::open()?.account_status()?,"active_operation":state.active.lock().unwrap().as_ref().map(|(id,_)| id.clone())}),
        ),
        Request::Search {
            query,
            account,
            limit,
        } => {
            let messages=Store::open()?.indexed_search(&query,account.as_deref(),limit)?.into_iter().map(|m|json!({"account":m.account,"id":m.id,"sender":m.sender,"email":m.email,"subject":m.subject,"date":m.date,"unread":m.unread,"snippet":m.body})).collect::<Vec<_>>();
            Ok(serde_json::to_value(messages)?)
        }
        Request::Changes {
            after,
            wait_seconds,
        } => {
            let revision = state.revision.lock().unwrap();
            let (revision, _) = state
                .changed
                .wait_timeout_while(
                    revision,
                    Duration::from_secs(wait_seconds.min(25)),
                    |revision| *revision == after,
                )
                .unwrap();
            Ok(json!({"instance":state.instance,"revision":*revision}))
        }
        Request::Read { account, id } => {
            let m = cached(&account, &id)?;
            Ok(
                json!({"account":m.account,"id":m.id,"sender":m.sender,"email":m.email,"subject":m.subject,"date":m.date,"to":m.to_header,"cc":m.cc_header,"body":m.body.chars().take(128_000).collect::<String>(),"truncated":m.body.chars().count()>128_000}),
            )
        }
        Request::Cancel { operation } => {
            if let Some((id, cancel)) = state.active.lock().unwrap().as_ref() {
                if id == &operation {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            Ok(json!("Cancellation requested"))
        }
        request => {
            // Interactive work takes priority over a resumable background scan.
            let interrupting_background = {
                let active = state.active.lock().unwrap();
                if let Some((id, cancel)) = active.as_ref() {
                    if id == "background-sync" {
                        cancel.store(true, Ordering::Relaxed);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            let deadline = Instant::now() + Duration::from_secs(35);
            let _guard = loop {
                match state.operation.try_lock() {
                    Ok(guard) => break guard,
                    Err(_) if interrupting_background && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        bail!("Mail service is busy; wait for the active operation to finish")
                    }
                }
            };
            let cancel = Arc::new(AtomicBool::new(false));
            *state.active.lock().unwrap() = Some((envelope.id, cancel.clone()));
            state.notify_change();
            let output = Mutex::new(stream.try_clone()?);
            let account = match &request {
                Request::Reconnect { email } => Some(email.clone()),
                Request::Modify { account, .. }
                | Request::Content { account, .. }
                | Request::Download { account, .. } => Some(account.clone()),
                Request::Send { draft, .. } => Some(draft.from.clone()),
                Request::Forward { source, .. } => Some(source.account.clone()),
                _ => None,
            };
            let follow_up = matches!(
                request,
                Request::Send { .. }
                    | Request::BulkModify { .. }
                    | Request::RemoveAccount { .. }
                    | Request::SetSyncHistory { .. }
                    | Request::Connect { .. }
                    | Request::ConnectProfile { .. }
                    | Request::Reconnect { .. }
            );
            let result = execute(request, cancel, |detail| {
                let _ = write_frame(
                    &mut output.lock().unwrap(),
                    &Response::Progress {
                        detail: detail.into(),
                    },
                );
            });
            if let (Some(account), Err(error)) = (account, &result) {
                if error.is::<ReconnectRequired>() {
                    let _ = Store::open()
                        .and_then(|s| s.record_status(&account, &Err(ReconnectRequired.into())));
                }
            }
            if follow_up && result.is_ok() {
                state
                    .sync_schedule
                    .lock()
                    .unwrap()
                    .request(Instant::now(), Duration::ZERO);
            }
            *state.active.lock().unwrap() = None;
            state.notify_change();
            result
        }
    }
}

pub fn run() -> Result<()> {
    let path = socket_path()?;
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("service.lock"))?;
    lock.try_lock()
        .context("A mail service is already running")?;
    Store::open()?.recover_interrupted_sync()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let state = Arc::new(State::default());
    let recovery = state.clone();
    crate::sync_recovery::watch_events(move |event| recovery.recovery_event(event));
    std::thread::spawn(|| loop {
        match Store::open().and_then(|mut s| s.index_search_batch()) {
            Ok(0) => std::thread::sleep(Duration::from_secs(2)),
            Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    });
    let background = state.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if background
            .sync_schedule
            .lock()
            .unwrap()
            .due(Instant::now())
            .is_none()
        {
            continue;
        }
        if let Ok(_guard) = background.operation.try_lock() {
            // Cache/read failures must not accidentally clear a user's pause preference.
            if Store::open().and_then(|s| s.sync_paused()).unwrap_or(true) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let generation = {
                let schedule = background.sync_schedule.lock().unwrap();
                let Some(generation) = schedule.due(Instant::now()) else {
                    continue;
                };
                *background.active.lock().unwrap() =
                    Some(("background-sync".into(), cancel.clone()));
                generation
            };
            background.notify_change();
            let result = backend::sync_background(cancel);
            let cancelled = result
                .as_ref()
                .err()
                .is_some_and(|e| e.is::<OperationCancelled>());
            *background.active.lock().unwrap() = None;
            background.sync_schedule.lock().unwrap().complete(
                generation,
                Instant::now(),
                result.is_err(),
                cancelled,
            );
            background.notify_change();
        }
    });
    // Keep a bounded number of connections, including idle/malformed clients.
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if connections.load(Ordering::Relaxed) >= 16 {
            continue;
        }
        connections.fetch_add(1, Ordering::Relaxed);
        let state = state.clone();
        let connections = connections.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, state);
            connections.fetch_sub(1, Ordering::Relaxed);
        });
    }
    drop(lock);
    Ok(())
}

pub fn available() -> bool {
    socket_path().is_ok_and(|path| UnixStream::connect(path).is_ok())
}
pub fn ensure_running() -> Result<()> {
    if UnixStream::connect(socket_path()?).is_ok() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--mail-service")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..50 {
        if UnixStream::connect(socket_path()?).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("Could not start mail service")
}
fn call(
    request: Request,
    cancel: Option<Arc<AtomicBool>>,
    progress: impl Fn(&str),
) -> Result<Value> {
    let id = format!("{:032x}", rand::random::<u128>());
    let mut stream = UnixStream::connect(socket_path()?)
        .context("Mail service unavailable; start focalmail --mail-service")?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write_frame(
        &mut stream,
        &Envelope {
            id: id.clone(),
            request,
        },
    )?;
    let complete = Arc::new(AtomicBool::new(false));
    if let Some(cancel) = cancel {
        let complete = complete.clone();
        let path = socket_path()?;
        std::thread::spawn(move || {
            while !complete.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    if let Ok(mut stream) = UnixStream::connect(path) {
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let _ = write_frame(
                            &mut stream,
                            &Envelope {
                                id: String::new(),
                                request: Request::Cancel { operation: id },
                            },
                        );
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
    let result = (|| {
        let mut reader = BufReader::new(stream);
        loop {
            match frame(&mut reader)? {
                Response::Progress { detail } => progress(&detail),
                Response::Done { value } => return Ok(value),
                Response::Error { detail } => bail!("{detail}"),
            }
        }
    })();
    complete.store(true, Ordering::Relaxed);
    result
}
pub fn command_line() -> Option<Result<()>> {
    let mut args = std::env::args().skip(1);
    match args.next()?.as_str() {
        "--mail-service" => Some(run()),
        "--mail-request" => Some((|| {
            let request: Request =
                serde_json::from_str(&args.next().context("Expected a JSON mail request")?)?;
            ensure_running()?;
            let result = call(request, None, |_| {})?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        })()),
        _ => None,
    }
}

pub mod client {
    use super::*;
    pub use backend::Action;
    fn text(request: Request, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
        Ok(serde_json::from_value(call(
            request,
            Some(cancel),
            progress,
        )?)?)
    }
    pub fn connect(path: &str, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
        text(Request::Connect { path: path.into() }, cancel, progress)
    }
    pub fn connect_profile(
        profile: crate::settings::Profile,
        password: String,
        cancel: Arc<AtomicBool>,
        progress: impl Fn(&str),
    ) -> Result<String> {
        text(
            Request::ConnectProfile { profile, password },
            cancel,
            progress,
        )
    }
    pub fn reconnect(
        email: &str,
        cancel: Arc<AtomicBool>,
        progress: impl Fn(&str),
    ) -> Result<String> {
        text(
            Request::Reconnect {
                email: email.into(),
            },
            cancel,
            progress,
        )
    }
    pub fn pause_background() -> Result<String> {
        Ok(serde_json::from_value(call(
            Request::PauseSync {},
            None,
            |_| {},
        )?)?)
    }
    pub fn sync_all(cancel: Arc<AtomicBool>) -> Result<String> {
        text(Request::Sync {}, cancel, |_| {})
    }
    pub fn bulk_modify(
        targets: Vec<crate::bulk::Target>,
        action: Action,
        cancel: Arc<AtomicBool>,
        progress: impl Fn(&str),
    ) -> Result<crate::bulk::Outcome> {
        Ok(serde_json::from_value(call(
            Request::BulkModify { targets, action },
            Some(cancel),
            progress,
        )?)?)
    }
    pub fn set_sync_history(
        account: String,
        history: crate::settings::SyncHistory,
        cancel: Arc<AtomicBool>,
    ) -> Result<String> {
        text(Request::SetSyncHistory { account, history }, cancel, |_| {})
    }
    pub fn remove_account(account: String, cancel: Arc<AtomicBool>) -> Result<String> {
        text(Request::RemoveAccount { account }, cancel, |_| {})
    }
    pub fn modify(message: &Message, action: Action, cancel: Arc<AtomicBool>) -> Result<String> {
        text(
            Request::Modify {
                account: message.account.clone(),
                id: message.id.clone(),
                action,
            },
            cancel,
            |_| {},
        )
    }
    pub fn send(
        draft: &Draft,
        files: &[DraftAttachment],
        cancel: Arc<AtomicBool>,
    ) -> Result<String> {
        text(
            Request::Send {
                draft: Box::new(draft.clone()),
                files: files.to_vec(),
            },
            cancel,
            |_| {},
        )
    }
    pub fn load_content(message: &Message) -> Result<Message> {
        Ok(serde_json::from_value(call(
            Request::Content {
                account: message.account.clone(),
                id: message.id.clone(),
            },
            None,
            |_| {},
        )?)?)
    }
    pub fn download(message: &Message, attachment: &Attachment) -> Result<String> {
        Ok(serde_json::from_value(call(
            Request::Download {
                account: message.account.clone(),
                id: message.id.clone(),
                attachment: attachment.clone(),
            },
            None,
            |_| {},
        )?)?)
    }
    pub fn forward_files(
        source: &ForwardSource,
        selected: &[usize],
        remaining: usize,
    ) -> Result<Vec<DraftAttachment>> {
        Ok(serde_json::from_value(call(
            Request::Forward {
                source: source.clone(),
                selected: selected.to_vec(),
                remaining,
            },
            None,
            |_| {},
        )?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_events_cancel_only_background_reads_and_keep_manual_pause() {
        let store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        store.set_sync_paused(true).unwrap();
        let state = State::default();
        let interactive = Arc::new(AtomicBool::new(false));
        *state.active.lock().unwrap() = Some(("interactive-send".into(), interactive.clone()));
        state.recovery_event(crate::sync_recovery::Event::Sleep(true));
        state.recovery_event(crate::sync_recovery::Event::Sleep(false));
        state.recovery_event(crate::sync_recovery::Event::NetworkRestored);
        assert!(!interactive.load(Ordering::Relaxed));
        assert!(store.sync_paused().unwrap());
        let background = Arc::new(AtomicBool::new(false));
        *state.active.lock().unwrap() = Some(("background-sync".into(), background.clone()));
        state.recovery_event(crate::sync_recovery::Event::NetworkLost);
        assert!(background.load(Ordering::Relaxed));
        assert!(state
            .sync_schedule
            .lock()
            .unwrap()
            .due(Instant::now() + Duration::from_secs(3))
            .is_some());
    }
    #[test]
    fn missing_credentials_require_sign_in_and_wrapped_network_errors_stay_offline() {
        assert!(credential_error(keyring::Error::NoEntry).is::<ReconnectRequired>());
        let error = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "test",
        ))
        .context("IMAP login interrupted");
        assert!(is_network_error(&error));
        assert!(!is_network_error(&ReconnectRequired.into()));
        assert!(!is_network_error(&anyhow::anyhow!(
            "Invalid server configuration"
        )));
    }
    #[test]
    fn pausing_background_sync_never_cancels_an_interactive_operation() {
        let store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let state = State::default();
        let interactive = Arc::new(AtomicBool::new(false));
        *state.active.lock().unwrap() = Some(("interactive-send".into(), interactive.clone()));
        pause_background(&state, &store).unwrap();
        assert!(store.sync_paused().unwrap());
        assert!(!interactive.load(Ordering::Relaxed));
        store.set_sync_paused(false).unwrap();
        let background = Arc::new(AtomicBool::new(false));
        *state.active.lock().unwrap() = Some(("background-sync".into(), background.clone()));
        pause_background(&state, &store).unwrap();
        assert!(background.load(Ordering::Relaxed));
    }

    #[test]
    fn interactive_request_cancels_background_sync_before_running() {
        let state = Arc::new(State::default());
        let background = state.clone();
        let (ready, started) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let guard = background.operation.lock().unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            *background.active.lock().unwrap() = Some(("background-sync".into(), cancel.clone()));
            background.notify_change();
            ready.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(cancel.load(Ordering::Relaxed));
            *background.active.lock().unwrap() = None;
            drop(guard);
        });
        started.recv().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Envelope {
                id: "interactive".into(),
                request: Request::Connect {
                    path: "/nonexistent-focalmail-test-client.json".into(),
                },
            },
        )
        .unwrap();
        handle(server, state).unwrap();
        match frame::<Response>(&mut BufReader::new(client)).unwrap() {
            Response::Error { detail } => {
                assert!(detail.contains("Cannot read Google desktop OAuth JSON"))
            }
            _ => panic!("Expected a missing-file error after taking priority over sync"),
        }
        worker.join().unwrap();
    }

    #[test]
    fn framing_rejects_truncated_and_unknown_requests() {
        assert!(frame::<Envelope>(&mut &b"{}"[..]).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"method":"status","token":"secret"}"#).is_err()
        );
    }
    #[test]
    fn status_and_cancel_work_while_provider_operation_is_locked() {
        let state = Arc::new(State::default());
        let cancel = Arc::new(AtomicBool::new(false));
        *state.active.lock().unwrap() = Some(("mine".into(), cancel.clone()));
        let _guard = state.operation.lock().unwrap();
        for (operation, expected) in [("other", false), ("mine", true)] {
            let (mut client, server) = UnixStream::pair().unwrap();
            write_frame(
                &mut client,
                &Envelope {
                    id: "cancel".into(),
                    request: Request::Cancel {
                        operation: operation.into(),
                    },
                },
            )
            .unwrap();
            handle(server, state.clone()).unwrap();
            assert!(matches!(
                frame::<Response>(&mut BufReader::new(client)).unwrap(),
                Response::Done { .. }
            ));
            assert_eq!(cancel.load(Ordering::Relaxed), expected);
        }
    }
    #[test]
    fn busy_operations_fail_without_executing() {
        let state = Arc::new(State::default());
        let _guard = state.operation.lock().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Envelope {
                id: "sync".into(),
                request: Request::Sync {},
            },
        )
        .unwrap();
        handle(server, state.clone()).unwrap();
        assert!(matches!(
            frame::<Response>(&mut BufReader::new(client)).unwrap(),
            Response::Error { .. }
        ));
    }
}
