//! Wire protocol between focaldmd and the greeter.
//!
//! Length-prefixed JSON over a Unix stream socket. The daemon owns the
//! listener; only the `focaldm` user may connect (enforced via SO_PEERCRED,
//! not filesystem permissions alone).

use std::path::Path;

use anyhow::{bail, Context as _};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use zeroize::Zeroizing;

/// Greeter -> daemon.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Begin a PAM transaction for `username`.
    CreateSession { username: String },
    /// Answer to the most recent `AuthMessage`.
    PostAuthResponse { response: Option<String> },
    /// Abort the in-flight PAM transaction.
    CancelSession,
}

/// Daemon -> greeter.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// PAM wants input or is showing a message.
    AuthMessage {
        style: AuthMessageStyle,
        message: String,
    },
    /// Authentication + account checks passed; session is being launched.
    SessionStarted,
    /// PAM transaction failed; greeter should show the error and reset.
    AuthError { message: String },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageStyle {
    /// prompt, echo off (passwords)
    Secret,
    /// prompt, echo on (e.g. OTP)
    Visible,
    Info,
    Error,
}

const MAX_FRAME: u32 = 64 * 1024;

pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub async fn recv(&mut self) -> anyhow::Result<Request> {
        let len = self.stream.read_u32_le().await.context("read frame len")?;
        if len > MAX_FRAME {
            bail!("frame too large: {len}");
        }
        // Zeroize the raw buffer: it may contain a password.
        let mut buf = Zeroizing::new(vec![0u8; len as usize]);
        self.stream
            .read_exact(&mut buf)
            .await
            .context("read frame body")?;
        Ok(serde_json::from_slice(&buf).context("decode request")?)
    }

    pub async fn send(&mut self, resp: &Response) -> anyhow::Result<()> {
        let body = serde_json::to_vec(resp)?;
        self.stream.write_u32_le(body.len() as u32).await?;
        self.stream.write_all(&body).await?;
        Ok(())
    }
}

pub struct Listener {
    inner: UnixListener,
    /// Only this uid may talk to us.
    allowed_uid: nix::unistd::Uid,
}

impl Listener {
    pub fn bind(path: &Path, allowed_uid: nix::unistd::Uid) -> anyhow::Result<Self> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let inner = UnixListener::bind(path).context("bind ipc socket")?;
        // Restrict to the greeter user; dir should already be root:root 0755,
        // socket becomes focaldm-readable only.
        nix::sys::stat::fchmodat(
            None,
            path,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
            nix::sys::stat::FchmodatFlags::FollowSymlink,
        )?;
        nix::unistd::chown(path, Some(allowed_uid), None)?;
        Ok(Self { inner, allowed_uid })
    }

    /// Accept the next connection from the greeter user, rejecting others.
    pub async fn accept_greeter(&self) -> anyhow::Result<Connection> {
        loop {
            let (stream, _) = self.inner.accept().await?;
            let cred =
                nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                    .context("SO_PEERCRED")?;
            if nix::unistd::Uid::from_raw(cred.uid()) == self.allowed_uid {
                return Ok(Connection { stream });
            }
            tracing::warn!(uid = cred.uid(), "rejected connection from non-greeter uid");
            // stream drops -> connection closed
        }
    }
}
