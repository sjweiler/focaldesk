//! Native Focaldesk IPC surface.
//!
//! Framing: 4-byte big-endian length prefix + JSON body (focaldm-ipc convention).
//! Transport: Unix stream socket, SO_PEERCRED-verified. Supports systemd socket
//! activation (LISTEN_FDS) or self-binding at $XDG_RUNTIME_DIR/focaldesk/secrets.sock.
//!
//! Broker keys (e.g. "google/oauth-refresh") map onto store items via the
//! reserved attribute `focald:key`, so the native surface and the
//! org.freedesktop.secrets surface see one store.

use crate::shared::Shared;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use zeroize::{Zeroize, Zeroizing};

pub const KEY_ATTR: &str = "focald:key";
const MAX_FRAME: u32 = 1 << 20; // 1 MiB
const MAX_SECRET_BYTES: usize = 700 * 1024;
const MAX_KEY_BYTES: usize = 512;
const MAX_ATTRIBUTES: usize = 128;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_CONNECTIONS: usize = 64;
const DEFAULT_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn io_timeout() -> std::time::Duration {
    std::env::var("FOCALD_SECRETS_IPC_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|milliseconds| (100..=60_000).contains(milliseconds))
        .map(std::time::Duration::from_millis)
        .unwrap_or(DEFAULT_IO_TIMEOUT)
}

#[derive(Default)]
struct SecretString(String);

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Ping,
    Get {
        key: String,
    },
    Set {
        key: String,
        value_b64: SecretString,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        attributes: BTreeMap<String, String>,
        #[serde(default)]
        content_type: Option<String>,
    },
    Delete {
        key: String,
    },
    List {
        #[serde(default)]
        prefix: Option<String>,
    },
}

/// Parse a native request frame without executing it. This is intentionally
/// public so fuzzing and external protocol conformance tests hit the exact
/// production deserializer.
pub fn validate_request_frame(frame: &[u8]) -> Result<(), serde_json::Error> {
    serde_json::from_slice::<Request>(frame).map(|_| ())
}

#[derive(Serialize)]
struct ListEntry {
    key: String,
    label: String,
    attributes: BTreeMap<String, String>,
    modified: u64,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
    Ok {
        ok: bool, // always true
        #[serde(skip_serializing_if = "Option::is_none")]
        value_b64: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entries: Option<Vec<ListEntry>>,
    },
    Err {
        ok: bool, // always false
        error: String,
    },
}

impl Drop for Response {
    fn drop(&mut self) {
        if let Response::Ok {
            value_b64: Some(value),
            ..
        } = self
        {
            zeroize::Zeroize::zeroize(value);
        }
    }
}

impl Response {
    fn ok() -> Self {
        Response::Ok {
            ok: true,
            value_b64: None,
            content_type: None,
            entries: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Response::Err {
            ok: false,
            error: msg.into(),
        }
    }
}

/// Take a socket from systemd (LISTEN_FDS) or bind our own.
pub fn make_listener() -> std::io::Result<UnixListener> {
    let listen_pid = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
    let listen_fds = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok());
    if listen_pid == Some(std::process::id()) && listen_fds == Some(1) {
        log::info!("ipc: using socket-activated fd 3");
        // SAFETY: systemd passes an inherited listening socket as fd 3 per sd_listen_fds(3).
        let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
        std_listener.set_nonblocking(true)?;
        return UnixListener::from_std(std_listener);
    }

    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR not set"))?
        .join("focaldesk");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("secrets.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    log::info!("ipc: listening on {}", path.display());
    Ok(listener)
}

pub async fn serve(listener: UnixListener, shared: Shared) {
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    log::warn!("ipc: connection limit ({MAX_CONNECTIONS}) reached");
                    drop(stream);
                    continue;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_conn(stream, shared).await {
                        log::debug!("ipc: connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                log::error!("ipc: accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_conn(mut stream: UnixStream, shared: Shared) -> std::io::Result<()> {
    let timeout = io_timeout();
    let cred = stream.peer_cred()?;
    // Hard invariant regardless of ACL contents: same-uid peers only.
    // SAFETY: geteuid is always safe to call.
    if cred.uid() != unsafe { libc::geteuid() } {
        log::warn!("ipc: rejecting cross-uid peer (uid {})", cred.uid());
        return Ok(());
    }
    let pid = cred.pid().unwrap_or(-1);
    // Pin the pid immediately: holding a pidfd keeps the kernel's struct pid
    // referenced, so the number cannot be recycled while we resolve identity
    // or serve requests. A peer that already exited gets rejected outright.
    // SAFETY: pidfd_open returns a new fd or -1; we take ownership on success.
    let _pidfd: OwnedFd = {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
        if fd < 0 {
            log::warn!("ipc: peer pid {pid} gone before identification; rejecting");
            return Ok(());
        }
        unsafe { OwnedFd::from_raw_fd(fd as i32) }
    };
    let identity = crate::acl::identify_peer(shared.dbus_conn().await.as_ref(), pid).await;
    log::debug!("ipc: peer pid={pid} identity={identity}");

    loop {
        let mut len_buf = [0u8; 4];
        match tokio::time::timeout(timeout, stream.read_exact(&mut len_buf)).await {
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "idle timeout",
                ))
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Ok(Err(e)) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf);
        if len == 0 || len > MAX_FRAME {
            return Err(std::io::Error::other("frame length out of bounds"));
        }
        let mut body = Zeroizing::new(vec![0u8; len as usize]);
        tokio::time::timeout(timeout, stream.read_exact(&mut body))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "frame read timeout")
            })??;

        let resp = match serde_json::from_slice::<Request>(&body) {
            Ok(req) => dispatch(req, &identity, &shared).await,
            Err(e) => Response::err(format!("bad request: {e}")),
        };
        let out = Zeroizing::new(serde_json::to_vec(&resp)?);
        let write_result = tokio::time::timeout(timeout, async {
            stream.write_all(&(out.len() as u32).to_be_bytes()).await?;
            stream.write_all(&out).await?;
            stream.flush().await
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "response timeout"))?;
        write_result?;
    }
}

async fn dispatch(req: Request, identity: &str, shared: &Shared) -> Response {
    // Hot-reload ACL if the file changed.
    shared.acl.lock().await.reload_if_changed();

    match req {
        Request::Ping => Response::ok(),

        Request::Get { key } => {
            if !valid_key(&key) {
                return Response::err("invalid key");
            }
            if !shared.acl.lock().await.check(identity, &key, false) {
                return denied(identity, &key, "get");
            }
            let store = shared.store.lock().await;
            let mut attrs = BTreeMap::new();
            attrs.insert(KEY_ATTR.to_string(), key.clone());
            match store.search(&attrs).first().and_then(|id| store.get(*id)) {
                Some(item) => Response::Ok {
                    ok: true,
                    value_b64: Some(base64::engine::general_purpose::STANDARD.encode(&item.secret)),
                    content_type: Some(item.content_type.clone()),
                    entries: None,
                },
                None => Response::err("not found"),
            }
        }

        Request::Set {
            key,
            mut value_b64,
            label,
            mut attributes,
            content_type,
        } => {
            if !valid_key(&key)
                || attributes.len() > MAX_ATTRIBUTES
                || label
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_METADATA_BYTES)
                || content_type
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_METADATA_BYTES)
                || attributes.iter().any(|(name, value)| {
                    name.len() > MAX_METADATA_BYTES || value.len() > MAX_METADATA_BYTES
                })
            {
                return Response::err("request metadata exceeds broker limits");
            }
            if !shared.acl.lock().await.check(identity, &key, true) {
                return denied(identity, &key, "set");
            }
            let decoded = base64::engine::general_purpose::STANDARD.decode(value_b64.0.as_bytes());
            value_b64.0.zeroize();
            let secret = match decoded {
                Ok(v) if v.len() <= MAX_SECRET_BYTES => v,
                Ok(mut v) => {
                    zeroize::Zeroize::zeroize(&mut v);
                    return Response::err("secret exceeds broker limits");
                }
                Err(e) => return Response::err(format!("value_b64: {e}")),
            };
            attributes.insert(KEY_ATTR.to_string(), key.clone());
            let mut store = shared.store.lock().await;
            let result = store.transaction(move |transaction| {
                // Replace by broker key rather than the full attribute set.
                let mut key_only = BTreeMap::new();
                key_only.insert(KEY_ATTR.to_string(), key.clone());
                for id in transaction.search(&key_only) {
                    transaction.delete(id);
                }
                transaction.create(
                    label.unwrap_or_else(|| key.clone()),
                    attributes,
                    secret,
                    content_type.unwrap_or_else(|| "text/plain".into()),
                    "org.freedesktop.Secret.Generic".into(),
                    false,
                )?;
                Ok(())
            });
            if let Err(e) = result {
                return Response::err(format!("persist failed: {e}"));
            }
            drop(store);
            shared.notify_dbus_items_changed().await;
            Response::ok()
        }

        Request::Delete { key } => {
            if !valid_key(&key) {
                return Response::err("invalid key");
            }
            if !shared.acl.lock().await.check(identity, &key, true) {
                return denied(identity, &key, "delete");
            }
            let mut store = shared.store.lock().await;
            let mut key_only = BTreeMap::new();
            key_only.insert(KEY_ATTR.to_string(), key.clone());
            let ids = store.search(&key_only);
            if ids.is_empty() {
                return Response::err("not found");
            }
            if let Err(e) = store.transaction(move |transaction| {
                for id in ids {
                    transaction.delete(id);
                }
                Ok(())
            }) {
                return Response::err(format!("persist failed: {e}"));
            }
            drop(store);
            shared.notify_dbus_items_changed().await;
            Response::ok()
        }

        Request::List { prefix } => {
            if prefix
                .as_ref()
                .is_some_and(|value| value.len() > MAX_KEY_BYTES)
            {
                return Response::err("invalid prefix");
            }
            let store = shared.store.lock().await;
            let acl = shared.acl.lock().await;
            let entries = store
                .items()
                .iter()
                .filter_map(|item| {
                    let key = item.attributes.get(KEY_ATTR)?;
                    if let Some(p) = &prefix {
                        if !key.starts_with(p.as_str()) {
                            return None;
                        }
                    }
                    if !acl.check(identity, key, false) {
                        return None;
                    }
                    let mut attrs = item.attributes.clone();
                    attrs.remove(KEY_ATTR);
                    Some(ListEntry {
                        key: key.clone(),
                        label: item.label.clone(),
                        attributes: attrs,
                        modified: item.modified,
                    })
                })
                .collect();
            Response::Ok {
                ok: true,
                value_b64: None,
                content_type: None,
                entries: Some(entries),
            }
        }
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && !key.bytes().any(|byte| byte.is_ascii_control())
}

fn denied(identity: &str, key: &str, op: &str) -> Response {
    log::warn!("acl: DENY {op} key={key} identity={identity}");
    Response::err("access denied")
}
