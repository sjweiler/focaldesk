// crates/focaldesk-ipc/src/lib.rs
use focaldesk_config::FocalDeskConfig;
use focaldesk_settings_core::{OutputConfig, Settings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

pub const DESKTOP_SOCKET_PATH: &str = "/tmp/focaldesk-desktop.sock";
pub const SOCKET_PATH: &str = DESKTOP_SOCKET_PATH;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    GetConfig,
    SetConfig {
        config: FocalDeskConfig,
    },
    GetAll,
    SetValue {
        path: String,
        value: Value,
    },
    SetDisplays {
        outputs: Vec<OutputConfig>,
    },
    IdentifyDisplays,
    Reload,
    ReloadConfig,
    Notify {
        title: String,
        body: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum IpcResponse {
    Ok,
    Notification { id: u64 },
    Config { config: FocalDeskConfig },
    Settings { settings: Settings },
    Error { message: String },
}

pub fn send_desktop_request(request: &IpcRequest) -> Result<IpcResponse, String> {
    let mut stream = UnixStream::connect(DESKTOP_SOCKET_PATH)
        .map_err(|err| format!("could not connect to {DESKTOP_SOCKET_PATH}: {err}"))?;
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

pub fn send_desktop_config(config: FocalDeskConfig) -> Result<(), String> {
    match send_desktop_request(&IpcRequest::SetConfig { config })? {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected IPC response: {other:?}")),
    }
}
