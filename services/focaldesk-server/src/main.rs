use anyhow::Result;

use focaldesk_ai::{serve_ai_ipc, Agent, AiService};
use focaldesk_logging::flog_info;
use std::sync::Arc;

mod ai_tools;
mod control_center;
mod web_ui;

use ai_tools::McpAgentTools;
use control_center::{control_center_socket_path, serve_control_center_ipc};

#[tokio::main]
async fn main() -> Result<()> {
    let _telemetry = focaldesk_logging::telemetry::init("focaldesk-server")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    flog_info!("FocalDesk server starting...");

    let agent = Agent::new("ipc server".to_string());
    flog_info!("Initialized agent: {}", agent.name);

    let ai_service = Arc::new(AiService::from_env()?.with_tool_executor(Arc::new(McpAgentTools)));
    if std::env::var("FOCALDESK_OLLAMA_PREWARM").as_deref() == Ok("1") {
        let service = ai_service.clone();
        tokio::spawn(async move {
            match service.warm_up_default_provider().await {
                Ok(()) => tracing::info!("default AI provider warm-up completed"),
                Err(error) => {
                    tracing::warn!(%error, "default AI provider warm-up failed; requests remain available")
                }
            }
        });
    }
    let scheduled_agents = ai_service.start_trigger_scheduler();
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
    flog_info!(
        "Initialized {} scheduled AI agent triggers",
        scheduled_agents
    );

    // later:
    // start IPC server
    // start automation runtime
    // load policies
    // handle client connections

    tokio::select! {
        result = async {
            tokio::try_join!(
                serve_ai_ipc(ai_service.clone()),
                serve_control_center_ipc(ai_service.clone()),
                web_ui::serve(ai_service.clone()),
            )
        } => { result?; }
        result = shutdown_signal() => { result?; }
    }
    Ok(())
}

// Return normally so the telemetry guard can flush its bounded export queue.
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}
