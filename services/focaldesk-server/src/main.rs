use anyhow::Result;

use focaldesk_ai::{serve_ai_ipc, Agent, AiService};
use focaldesk_logging::flog_info;
use std::sync::Arc;

mod control_center;

use control_center::{control_center_socket_path, serve_control_center_ipc};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk server starting...");

    let agent = Agent::new("ipc server".to_string());
    flog_info!("Initialized agent: {}", agent.name);

    let ai_service = Arc::new(AiService::from_env()?);
    flog_info!(
        "AI IPC listening on {}; default provider: {}; providers: {}",
        focaldesk_ai::ai_socket_path()?.display(),
        ai_service.default_provider(),
        ai_service
            .providers()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    flog_info!(
        "Control Center IPC listening on {}",
        control_center_socket_path()?.display()
    );

    // later:
    // start IPC server
    // start automation runtime
    // load policies
    // handle client connections

    tokio::try_join!(
        serve_ai_ipc(ai_service.clone()),
        serve_control_center_ipc(ai_service),
    )?;
    Ok(())
}
