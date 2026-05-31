use anyhow::Result;

use flowstate_ai::Agent;
use flowstate_engine::backend;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("FlowState server starting...");

    let agent = Agent::new("ipc server".to_string());
    println!("Initialized agent: {}", agent.name);

    // later:
    // start IPC server
    // start automation runtime
    // load policies
    // handle client connections

    Ok(())
}
