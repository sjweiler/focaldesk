use anyhow::Result;
use focaldesk_ipc::serve_notification_ipc;
use focaldesk_logging::flog_info;
use focaldesk_notifications::NotificationManager;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk notifications daemon starting...");
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let state_path = state_home.join("focaldesk/notifications.json");
    let mut manager = NotificationManager::new();
    let settings = focaldesk_settings_core::load_settings();
    manager.set_history_limit(settings.privacy.notification_history_limit as usize);
    manager.load_history(&state_path);
    serve_notification_ipc(Arc::new(Mutex::new(manager)), state_path);
    std::thread::park();
    Ok(())
}
