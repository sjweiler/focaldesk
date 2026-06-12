// crates/focaldesk-ipc/src/lib.rs
use focaldesk_settings_core::{OutputConfig, Settings};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SOCKET_PATH: &str = "/tmp/focaldesk-settings.sock";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    GetAll,
    SetValue { path: String, value: Value },
    SetDisplays { outputs: Vec<OutputConfig> },
    IdentifyDisplays,
    Reload,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum IpcResponse {
    Ok,
    Settings { settings: Settings },
    Error { message: String },
}
