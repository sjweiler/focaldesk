use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    thread,
};

pub const DIALOG_SOCKET_PATH: &str = "/tmp/focaldesk-dialogd.sock";

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
    let path = dialog_socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|err| format!("could not connect to {path}: {err}"))?;
    let json = serde_json::to_vec(request).map_err(|err| err.to_string())?;

    stream.write_all(&json).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    serde_json::from_str(&response).map_err(|err| err.to_string())
}

pub fn serve_dialog_ipc(
    app: Arc<dyn Fn(DialogIpcRequest) -> DialogIpcResponse + Send + Sync + 'static>,
) {
    let _ = std::fs::remove_file(DIALOG_SOCKET_PATH);
    let listener =
        UnixListener::bind(DIALOG_SOCKET_PATH).expect("failed to bind FocalDesk dialog IPC socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let app = app.clone();
                thread::spawn(move || handle_dialog_client(&mut stream, app));
            }
        }
    });
}

fn handle_dialog_client(
    stream: &mut UnixStream,
    app: Arc<dyn Fn(DialogIpcRequest) -> DialogIpcResponse + Send + Sync + 'static>,
) {
    let mut buf = String::new();

    if stream.read_to_string(&mut buf).is_err() {
        return;
    }

    let response = match serde_json::from_str::<DialogIpcRequest>(&buf) {
        Ok(request) => (app)(request),
        Err(err) => DialogIpcResponse::Error {
            message: err.to_string(),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let _ = stream.write_all(json.as_bytes());
}

fn dialog_socket_path() -> String {
    std::env::var("FOCALDESK_DIALOG_SOCKET_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DIALOG_SOCKET_PATH.to_string())
}
