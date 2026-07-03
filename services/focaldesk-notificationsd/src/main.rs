use anyhow::Result;
use focaldesk_ipc::serve_notification_ipc;
use focaldesk_logging::flog_info;
use focaldesk_notifications::NotificationManager;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk notifications daemon starting...");
    serve_notification_ipc(Arc::new(Mutex::new(NotificationManager::new())));
    std::thread::park();
    Ok(())
}
