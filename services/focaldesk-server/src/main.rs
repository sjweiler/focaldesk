use anyhow::Result;

use focaldesk_ai::Agent;
use focaldesk_logging::flog_info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk server starting...");

    let agent = Agent::new("ipc server".to_string());
    flog_info!("Initialized agent: {}", agent.name);

    // later:
    // start IPC server
    // start automation runtime
    // load policies
    // handle client connections

    Ok(())
}
