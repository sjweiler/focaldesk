use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::config::{self, AutomationEntry};
use crate::script;

#[derive(Debug, Clone, Default)]
pub struct RunState {
    pub last_run_unix: Option<i64>,
    pub last_error: Option<String>,
}

pub type SharedRunState = Arc<Mutex<BTreeMap<String, RunState>>>;

/// Runs `automation`'s script on a blocking thread and records the outcome
/// in `state`. Shared by the interval/daily scheduler loop and the IPC
/// `RunNow` handler so both paths report through the same status map.
pub async fn run_and_record(automation: &AutomationEntry, state: &SharedRunState) {
    let path = config::script_path(&automation.script);
    let name = automation.name.clone();

    tracing::info!(target: "focaldesk.automation", automation = %name, path = %path.display(), "running automation");

    let result = tokio::task::spawn_blocking(move || script::run_script(&name, &path))
        .await
        .unwrap_or_else(|join_err| Err(format!("automation task panicked: {join_err}")));

    if let Err(err) = &result {
        tracing::warn!(target: "focaldesk.automation", automation = %automation.name, error = %err, "automation run failed");
    }

    let mut state = state.lock().expect("automation run state lock poisoned");
    state.insert(
        automation.name.clone(),
        RunState {
            last_run_unix: Some(now_unix()),
            last_error: result.err(),
        },
    );
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
