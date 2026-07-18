use focaldesk_automation::{config, ipc, runner};
use focaldesk_automation::{runner::SharedRunState, schedule};
use focaldesk_logging::{init_default_logging, session_id};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    init_default_logging();
    info!(
        target: "focaldesk",
        session_id = session_id(),
        "automation daemon started"
    );

    let automation_config = config::load_config();
    info!(
        target: "focaldesk.automation",
        count = automation_config.automations.len(),
        path = %config::config_path().display(),
        "loaded automations"
    );

    let state: SharedRunState = Arc::new(Mutex::new(BTreeMap::new()));

    for automation in automation_config
        .automations
        .iter()
        .filter(|automation| automation.enabled)
        .cloned()
        .collect::<Vec<_>>()
    {
        let state = state.clone();
        tokio::spawn(run_on_schedule(automation, state));
    }

    let config = Arc::new(automation_config);
    if let Err(err) = ipc::serve(config, state).await {
        warn!(target: "focaldesk.automation", error = %err, "automation IPC server exited");
    }
}

async fn run_on_schedule(automation: config::AutomationEntry, state: SharedRunState) {
    let parsed = match schedule::Schedule::parse(&automation.schedule) {
        Ok(schedule) => schedule,
        Err(err) => {
            warn!(
                target: "focaldesk.automation",
                automation = %automation.name,
                schedule = %automation.schedule,
                error = %err,
                "invalid schedule, this automation will never run on its own (RunNow still works)"
            );
            return;
        }
    };

    loop {
        tokio::time::sleep(parsed.next_delay()).await;
        runner::run_and_record(&automation, &state).await;
    }
}
