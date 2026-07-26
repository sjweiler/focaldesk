use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
};

use crate::transport;

pub const CONTROL_SOCKET_NAME: &str = "controls.sock";
pub const CONTROL_SOCKET_ENV: &str = "FOCALDESK_CONTROL_SOCKET_PATH";

pub fn control_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(CONTROL_SOCKET_ENV, CONTROL_SOCKET_NAME)
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
    let path = control_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|err| format!("could not connect to {}: {err}", path.display()))?;
    transport::configure_stream(&stream).map_err(|err| err.to_string())?;
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

pub fn serve_control_ipc(
    handler: Arc<dyn Fn(ControlIpcRequest) -> ControlIpcResponse + Send + Sync + 'static>,
) {
    let path = control_socket_path().expect("could not resolve FocalDesk control IPC socket");
    let listener =
        transport::bind_user_socket(&path).expect("failed to bind FocalDesk control IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let handler = handler.clone();
            thread::spawn(move || handle_control_client(&mut stream, &handler));
        }
    });
}

fn handle_control_client(
    stream: &mut UnixStream,
    handler: &Arc<dyn Fn(ControlIpcRequest) -> ControlIpcResponse + Send + Sync + 'static>,
) {
    if transport::require_authorized_peer(stream, transport::CONTROL_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<ControlIpcRequest>(&buf) {
        Ok(request) => handler(request),
        Err(e) => ControlIpcResponse::Error {
            message: e.to_string(),
        },
    };

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}
