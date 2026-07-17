//! Client side of the focaldmd protocol.
//!
//! Mirror of the daemon's ipc.rs: length-prefixed JSON over the Unix socket
//! at $FOCALDM_SOCKET. The socket is non-blocking and designed to sit in
//! calloop as a Generic fd source alongside DRM and libinput — the greeter
//! must keep rendering (cursor, caret blink, spinner) while PAM thinks.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{bail, Context as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Greeter -> daemon. Must match focaldmd's `Request` exactly.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession { username: String },
    PostAuthResponse { response: Option<String> },
    CancelSession,
    Power { action: PowerAction },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Suspend,
    Hibernate,
    Restart,
    PowerOff,
}

/// Daemon -> greeter. Must match focaldmd's `Response` exactly.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    AuthMessage {
        style: AuthMessageStyle,
        message: String,
    },
    SessionStarted,
    AuthError {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageStyle {
    Secret,
    Visible,
    Info,
    Error,
}

const MAX_FRAME: u32 = 64 * 1024;

pub struct DaemonConnection {
    stream: UnixStream,
    /// Bytes read but not yet framed. Zeroized on drop and after each
    /// drained frame — inbound traffic is not sensitive today, but keep
    /// the hygiene symmetric with the daemon.
    inbox: Vec<u8>,
    /// Bytes queued for writing. Zeroized as they drain — outbound frames
    /// carry the password.
    outbox: VecDeque<u8>,
}

impl DaemonConnection {
    /// Connect to $FOCALDM_SOCKET (or an explicit path in tests).
    pub fn connect(path: &Path) -> anyhow::Result<Self> {
        let stream =
            UnixStream::connect(path).with_context(|| format!("connect {}", path.display()))?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            inbox: Vec::with_capacity(4096),
            outbox: VecDeque::new(),
        })
    }

    /// Wrap an already-connected stream (tests, socket activation).
    pub fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            inbox: Vec::with_capacity(4096),
            outbox: VecDeque::new(),
        }
    }

    /// The fd to register with calloop (Interest::READ, plus WRITE while
    /// `wants_write()` is true).
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub fn wants_write(&self) -> bool {
        !self.outbox.is_empty()
    }

    /// Queue a request. Call `flush()` afterwards (and on every writable
    /// event) to actually push bytes.
    pub fn send(&mut self, req: &Request) -> anyhow::Result<()> {
        let mut body = serde_json::to_vec(req)?;
        if body.len() as u32 > MAX_FRAME {
            body.zeroize();
            bail!("frame too large");
        }
        self.outbox.extend((body.len() as u32).to_le_bytes());
        self.outbox.extend(body.iter());
        body.zeroize(); // password lived here
        Ok(())
    }

    /// Drain as much of the outbox as the socket will take.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        while !self.outbox.is_empty() {
            let (front, _) = self.outbox.as_slices();
            match self.stream.write(front) {
                Ok(0) => bail!("daemon closed the socket"),
                Ok(n) => {
                    // Zeroize what we just sent before dropping it.
                    for b in self.outbox.drain(..n) {
                        let mut b = b;
                        b.zeroize();
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Read everything available and return complete frames. Call on every
    /// readable event. Ok(vec![]) just means "no full frame yet".
    pub fn read_responses(&mut self) -> anyhow::Result<Vec<Response>> {
        let mut scratch = [0u8; 4096];
        loop {
            match self.stream.read(&mut scratch) {
                Ok(0) => bail!("daemon closed the socket"),
                Ok(n) => self.inbox.extend_from_slice(&scratch[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }

        let mut out = Vec::new();
        loop {
            if self.inbox.len() < 4 {
                break;
            }
            let len = u32::from_le_bytes(self.inbox[..4].try_into().unwrap());
            if len > MAX_FRAME {
                bail!("oversized frame from daemon: {len}");
            }
            let end = 4 + len as usize;
            if self.inbox.len() < end {
                break;
            }
            let resp: Response =
                serde_json::from_slice(&self.inbox[4..end]).context("decode response")?;
            out.push(resp);
            self.inbox[..end].zeroize();
            self.inbox.drain(..end);
        }
        Ok(out)
    }
}

impl Drop for DaemonConnection {
    fn drop(&mut self) {
        self.inbox.zeroize();
        self.outbox.make_contiguous().zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_power_request_for_daemon() {
        let value = serde_json::to_value(Request::Power {
            action: PowerAction::Suspend,
        })
        .unwrap();
        assert_eq!(value["type"], "power");
        assert_eq!(value["action"], "suspend");
    }
}
