// crates/focaldesk-ipc/src/lib.rs
pub mod controls;
pub mod dialog;
pub mod notifications;
pub mod power;
pub mod settings;

use focaldesk_config::FocalDeskConfig;
use focaldesk_power::PowerSnapshot;
use focaldesk_settings_core::{OutputConfig, Settings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

pub const DESKTOP_SOCKET_PATH: &str = "/tmp/focaldesk-desktop.sock";
pub const SOCKET_PATH: &str = DESKTOP_SOCKET_PATH;
pub const SETTINGS_SOCKET_PATH: &str = "/tmp/focaldesk-settings.sock";

fn desktop_socket_path() -> String {
    std::env::var("FOCALDESK_DESKTOP_SOCKET_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DESKTOP_SOCKET_PATH.to_string())
}

fn settings_socket_path() -> String {
    std::env::var("FOCALDESK_SETTINGS_SOCKET_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| SETTINGS_SOCKET_PATH.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    Get {
        key: String,
    },
    Set {
        key: String,
        value: Value,
    },
    Watch {
        keys: Vec<String>,
    },
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
    GetDisplayRuntimeStatus,
    GetPowerSnapshot,
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
    Value {
        key: String,
        value: Value,
    },
    Event {
        key: String,
        value: Value,
    },
    Notification {
        id: u64,
    },
    Config {
        config: FocalDeskConfig,
    },
    Settings {
        settings: Settings,
    },
    DisplayRuntimeStatus {
        outputs: Vec<DisplayRuntimeOutputStatus>,
    },
    PowerSnapshot {
        snapshot: PowerSnapshot,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayRuntimeOutputStatus {
    pub connector: String,
    #[serde(default)]
    pub icc_lut_fallback_active: bool,
    #[serde(default)]
    pub wide_gamut_active: bool,
}

pub fn send_desktop_request(request: &IpcRequest) -> Result<IpcResponse, String> {
    let path = desktop_socket_path();
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

pub fn send_settings_request(request: &IpcRequest) -> Result<IpcResponse, String> {
    let path = settings_socket_path();
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

pub fn send_desktop_get(key: impl Into<String>) -> Result<Value, String> {
    let key = key.into();
    match send_desktop_request(&IpcRequest::Get { key: key.clone() })? {
        IpcResponse::Value { value, .. } => Ok(value),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected IPC response for {key}: {other:?}")),
    }
}

pub fn send_desktop_set(key: impl Into<String>, value: Value) -> Result<(), String> {
    let key = key.into();
    match send_desktop_request(&IpcRequest::Set { key, value })? {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected IPC response: {other:?}")),
    }
}

pub fn watch_desktop_keys(
    keys: Vec<String>,
    mut on_response: impl FnMut(IpcResponse),
) -> Result<(), String> {
    let path = desktop_socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|err| format!("could not connect to {path}: {err}"))?;
    let json = serde_json::to_vec(&IpcRequest::Watch { keys }).map_err(|err| err.to_string())?;

    stream.write_all(&json).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|err| err.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let response = serde_json::from_str(&line).map_err(|err| err.to_string())?;
        on_response(response);
    }

    Ok(())
}

pub fn send_desktop_config(config: FocalDeskConfig) -> Result<(), String> {
    match send_desktop_request(&IpcRequest::SetConfig { config })? {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected IPC response: {other:?}")),
    }
}

pub use controls::{
    CONTROL_SOCKET_PATH, ControlIpcRequest, ControlIpcResponse, ControlSetting,
    send_control_request, serve_control_ipc,
};
pub use dialog::{
    DIALOG_SOCKET_PATH, DialogIpcRequest, DialogIpcResponse, send_dialog_request, serve_dialog_ipc,
};
pub use notifications::{
    NOTIFICATIONS_SOCKET_PATH, NotificationIpcRequest, NotificationIpcResponse,
    send_notification_request, serve_notification_ipc,
};
pub use power::{
    POWER_SOCKET_PATH, PowerIpcRequest, PowerIpcResponse, send_power_request, serve_power_ipc,
};
pub use settings::serve_settings_ipc;
