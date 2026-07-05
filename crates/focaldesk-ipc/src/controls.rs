use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    thread,
};

pub const CONTROL_SOCKET_PATH: &str = "/tmp/focaldesk-controls.sock";

fn control_socket_path() -> String {
    std::env::var("FOCALDESK_CONTROL_SOCKET_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CONTROL_SOCKET_PATH.to_string())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlSetting {
    Wifi,
    Bluetooth,
    DoNotDisturb,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlIpcRequest {
    SetSystemSetting {
        setting: ControlSetting,
        enabled: bool,
    },
    SetVolume {
        volume: f32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ControlIpcResponse {
    Ok,
    Error { message: String },
}

pub fn send_control_request(request: &ControlIpcRequest) -> Result<ControlIpcResponse, String> {
    let path = control_socket_path();
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

pub fn serve_control_ipc(
    handler: Arc<dyn Fn(ControlIpcRequest) -> ControlIpcResponse + Send + Sync + 'static>,
) {
    let _ = std::fs::remove_file(CONTROL_SOCKET_PATH);

    let listener = UnixListener::bind(CONTROL_SOCKET_PATH)
        .expect("failed to bind FocalDesk control IPC socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let handler = handler.clone();
                thread::spawn(move || handle_control_client(&mut stream, &handler));
            }
        }
    });
}

fn handle_control_client(
    stream: &mut UnixStream,
    handler: &Arc<dyn Fn(ControlIpcRequest) -> ControlIpcResponse + Send + Sync + 'static>,
) {
    let mut buf = String::new();

    if stream.read_to_string(&mut buf).is_err() {
        return;
    }

    let response = match serde_json::from_str::<ControlIpcRequest>(&buf) {
        Ok(request) => handler(request),
        Err(e) => ControlIpcResponse::Error {
            message: e.to_string(),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let _ = stream.write_all(json.as_bytes());
}
