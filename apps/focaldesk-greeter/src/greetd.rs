use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop;

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    AuthError,
    Error,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error {
        error_type: ErrorType,
        description: String,
    },
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },
}

fn write_message(sock: &mut UnixStream, req: &Request) -> anyhow::Result<()> {
    let body = serde_json::to_vec(req)?;
    sock.write_all(&(body.len() as u32).to_le_bytes())?;
    sock.write_all(&body)?;
    Ok(())
}

fn read_message(sock: &mut UnixStream) -> anyhow::Result<Response> {
    let mut len_buf = [0u8; 4];
    sock.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    sock.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

/// Owns the blocking greetd socket on its own thread, since greetd IPC is
/// request/response over a Unix socket and the compositor's calloop event
/// loop must not stall on it. The caller sends `Request`s in over `requests`
/// and receives `Response`s (or IO/protocol errors) back over `responses`.
pub fn spawn(
    requests: mpsc::Receiver<Request>,
    responses: calloop::channel::Sender<anyhow::Result<Response>>,
) -> anyhow::Result<thread::JoinHandle<()>> {
    let path = std::env::var("GREETD_SOCK")
        .map_err(|_| anyhow::anyhow!("GREETD_SOCK not set — not running under greetd"))?;
    let mut sock = UnixStream::connect(path)?;

    Ok(thread::spawn(move || {
        while let Ok(req) = requests.recv() {
            let result = write_message(&mut sock, &req).and_then(|_| read_message(&mut sock));
            let stop = result.is_err();
            if responses.send(result).is_err() || stop {
                break;
            }
        }
    }))
}
