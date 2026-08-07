// crates/focaldesk-ipc/src/lib.rs
pub mod controls;
pub mod dialog;
pub mod notifications;
pub mod power;
pub mod settings;
pub mod transport;

use focaldesk_config::FocalDeskConfig;
use focaldesk_power::PowerSnapshot;
use focaldesk_settings_core::{OutputConfig, Settings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

pub const DESKTOP_SOCKET_NAME: &str = "desktop.sock";
pub const SETTINGS_SOCKET_NAME: &str = "settings.sock";
pub const DESKTOP_SOCKET_ENV: &str = "FOCALDESK_DESKTOP_SOCKET_PATH";
pub const SETTINGS_SOCKET_ENV: &str = "FOCALDESK_SETTINGS_SOCKET_PATH";

pub fn desktop_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(DESKTOP_SOCKET_ENV, DESKTOP_SOCKET_NAME)
}

pub fn settings_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(SETTINGS_SOCKET_ENV, SETTINGS_SOCKET_NAME)
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
    /// Returns a bounded, secret-free snapshot of compositor-owned desktop state.
    GetDesktopSnapshot,
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
    ExecuteDesktopAction {
        action: DesktopAction,
    },
    /// Sent by a trusted shell client after its first configured frame.
    ShellReady {
        namespace: String,
        output_count: usize,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Actions that trusted desktop helpers may ask the compositor to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DesktopAction {
    LaunchApp { app: String },
    FocusWorkspace { workspace: u32 },
    MoveFocusedToOutput { output: u32 },
    MoveFocused { direction: DesktopDirection },
    CloseFocused,
    SetVolume { percent: u8 },
    FocusWindow { window_id: u32 },
    MoveWindowToWorkspace { window_id: u32, workspace: u32 },
    OpenSettingsPanel { panel: String },
    OpenNotificationsPanel,
    ToggleDoNotDisturb,
    CreateWorkspace,
    DeleteWorkspace,
    OpenCalendarPanel,
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
    DesktopSnapshot {
        snapshot: DesktopSnapshot,
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

/// MCP and diagnostic consumers receive this typed projection rather than
/// reaching into compositor state directly. It intentionally contains only
/// desktop metadata and must never grow credential or clipboard fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSnapshot {
    pub session: SessionStatus,
    #[serde(default)]
    pub shell: ShellSnapshot,
    pub outputs: Vec<OutputSnapshot>,
    pub windows: Vec<WindowSnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub rendering: RenderingStatus,
}

/// Small, secret-free state projection intended for trusted shell clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShellSnapshot {
    pub workspace_count: usize,
    pub do_not_disturb: bool,
    pub notification_unread_count: usize,
    pub network_carrier: bool,
    pub wifi_signal_percent: Option<u8>,
    pub battery_percent: Option<u8>,
    pub focused_window_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub running: bool,
    pub locked: bool,
    pub focused_output_id: u64,
    pub focused_window_id: Option<u32>,
    pub active_workspace_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSnapshot {
    pub id: u64,
    pub connector: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub active_workspace_id: u32,
    pub focused: bool,
    pub hdr_supported: bool,
    pub hdr_requested: bool,
    pub hdr_active: bool,
    pub wide_gamut_active: bool,
    pub icc_lut_fallback_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub id: u32,
    pub title: String,
    pub app_id: Option<String>,
    pub class: Option<String>,
    pub workspace_id: u32,
    pub output_id: Option<u64>,
    pub mapped: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub focused: bool,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: u32,
    pub name: String,
    pub active_on_output_ids: Vec<u64>,
    pub window_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderingStatus {
    pub backend: String,
    pub compositor_ready: bool,
    pub output_count: usize,
    pub damage_debug_enabled: bool,
}

pub fn send_desktop_request(request: &IpcRequest) -> Result<IpcResponse, String> {
    let path = desktop_socket_path()?;
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

pub fn send_settings_request(request: &IpcRequest) -> Result<IpcResponse, String> {
    let path = settings_socket_path()?;
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
    let path = desktop_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|err| format!("could not connect to {}: {err}", path.display()))?;
    transport::configure_stream(&stream).map_err(|err| err.to_string())?;
    let json = transport::encode_message(&IpcRequest::Watch { keys })?;

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
        let response = transport::decode_message(line.as_bytes())?;
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
    CONTROL_SOCKET_ENV, CONTROL_SOCKET_NAME, ControlIpcRequest, ControlIpcResponse, ControlSetting,
    control_socket_path, send_control_request, serve_control_ipc,
};
pub use dialog::{
    DIALOG_SOCKET_ENV, DIALOG_SOCKET_NAME, DialogIpcRequest, DialogIpcResponse, dialog_socket_path,
    send_dialog_request, serve_dialog_ipc,
};
pub use notifications::{
    NOTIFICATIONS_SOCKET_ENV, NOTIFICATIONS_SOCKET_NAME, NotificationIpcRequest,
    NotificationIpcResponse, notifications_socket_path, send_notification_request,
    serve_notification_ipc,
};
pub use power::{
    POWER_SOCKET_ENV, POWER_SOCKET_NAME, PowerIpcRequest, PowerIpcResponse, power_socket_path,
    send_power_request, serve_power_ipc,
};
pub use settings::serve_settings_ipc;
