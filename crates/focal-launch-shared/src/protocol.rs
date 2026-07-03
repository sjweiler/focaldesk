// focal_launch/src/protocol.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub trace_id: u64,
    pub app: String,
    pub args: Vec<String>,
    pub wayland_display: String,
    pub xwayland_display: Option<String>,
    pub browser_backend: BrowserBackend,
    pub source: LaunchSource,
    #[serde(default)]
    pub clear_chrome_profile: Option<String>,
    #[serde(default)]
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BrowserBackend {
    Auto,
    Wayland,
    Xwayland,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LaunchSource {
    Ui,
    Keybind,
    Ai,
    Plugin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LaunchResponse {
    Accepted,
    Failed { message: String },
}
