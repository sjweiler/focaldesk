use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
    time::Duration,
};

use crate::transport;

pub const DIALOG_SOCKET_NAME: &str = "dialog.sock";
pub const DIALOG_SOCKET_ENV: &str = "FOCALDESK_DIALOG_SOCKET_PATH";
/// Dialogs wait for a person, so the normal five-second IPC timeout is much too
/// short. Keep a finite bound for abandoned brokers while allowing password
/// managers and accessibility input enough time to answer.
const DIALOG_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DialogIpcRequest {
    AiPermissionPrompt {
        request_id: u64,
        title: String,
        message: String,
        #[serde(default)]
        allow_persistent: bool,
    },
    PortalChooserPrompt {
        request_id: u64,
        title: String,
        message: String,
        choices: Vec<String>,
    },
    /// One prompt in a PolicyKit authentication conversation (`polkit_agent::Session`'s
    /// `request` signal) — typically "Password: " but PAM can ask other things.
    PolkitAuthPrompt {
        request_id: u64,
        message: String,
        icon_name: String,
        prompt: String,
        echo_on: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum DialogIpcResponse {
    AiPermissionDecision {
        request_id: u64,
        allow: bool,
        persistent: bool,
    },
    PortalChooserDecision {
        request_id: u64,
        selected: Option<String>,
    },
    /// `answer` is `None` when the user cancelled/dismissed the prompt.
    PolkitAuthAnswer {
        request_id: u64,
        answer: Option<String>,
    },
    Error {
        message: String,
    },
}

pub fn send_dialog_request(request: &DialogIpcRequest) -> Result<DialogIpcResponse, String> {
    let path = dialog_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|err| format!("could not connect to {}: {err}", path.display()))?;
    transport::configure_stream(&stream).map_err(|err| err.to_string())?;
    stream
        .set_read_timeout(Some(DIALOG_RESPONSE_TIMEOUT))
        .map_err(|err| err.to_string())?;
    let json = transport::encode_message(request)?;

    stream.write_all(&json).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    transport::decode_message(response.as_bytes())
}

pub fn serve_dialog_ipc(
    app: Arc<dyn Fn(DialogIpcRequest) -> DialogIpcResponse + Send + Sync + 'static>,
) {
    let path = dialog_socket_path().expect("could not resolve FocalDesk dialog IPC socket");
    let listener =
        transport::bind_user_socket(&path).expect("failed to bind FocalDesk dialog IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let app = app.clone();
            thread::spawn(move || handle_dialog_client(&mut stream, app));
        }
    });
}

fn handle_dialog_client(
    stream: &mut UnixStream,
    app: Arc<dyn Fn(DialogIpcRequest) -> DialogIpcResponse + Send + Sync + 'static>,
) {
    if transport::require_authorized_peer(stream, transport::DIALOG_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<DialogIpcRequest>(&buf) {
        Ok(request) => (app)(request),
        Err(err) => DialogIpcResponse::Error {
            message: err.to_string(),
        },
    };

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}

pub fn dialog_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(DIALOG_SOCKET_ENV, DIALOG_SOCKET_NAME)
}
