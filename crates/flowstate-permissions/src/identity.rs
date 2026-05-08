#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AppIdentity {
    DesktopId(String),      // org.mozilla.firefox
    FlatpakId(String),      // org.signal.Signal
    WaylandAppId(String),   // xdg_toplevel app_id
    ExecutablePath(String), // fallback
    Unknown,
}

#[derive(Debug, Clone)]
pub struct AppMetadata {
    pub identity: AppIdentity,
    pub pid: Option<u32>,
    pub window_title: Option<String>,
    pub sandboxed: bool,
}
