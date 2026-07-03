use anyhow::Result;
use focaldesk_ipc::serve_power_ipc;
use focaldesk_logging::flog_info;
use focaldesk_power::PowerManager;
use std::sync::Arc;

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk power daemon starting...");
    serve_power_ipc(Arc::new(PowerManager::new()));
    std::thread::park();
    Ok(())
}
