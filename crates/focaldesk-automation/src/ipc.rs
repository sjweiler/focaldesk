use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config::AutomationConfig;
use crate::runner::{self, SharedRunState};

pub const AUTOMATION_SOCKET_NAME: &str = "focaldesk-automation.sock";
pub const AUTOMATION_SOCKET_ENV: &str = "FOCALDESK_AUTOMATION_SOCKET";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutomationIpcRequest {
    ListAutomations,
    RunNow { name: String },
    Status,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AutomationIpcResponse {
    Automations {
        automations: Vec<AutomationSummary>,
    },
    RanOk {
        name: String,
    },
    Status {
        automation_count: usize,
        config_path: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationSummary {
    pub name: String,
    pub schedule: String,
    pub enabled: bool,
    pub last_run_unix: Option<i64>,
    pub last_error: Option<String>,
}

pub fn automation_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os(AUTOMATION_SOCKET_ENV) {
        return PathBuf::from(path);
    }

    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(AUTOMATION_SOCKET_NAME)
}

pub async fn serve(config: Arc<AutomationConfig>, state: SharedRunState) -> Result<()> {
    let path = automation_socket_path();
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind automation IPC socket {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("automation IPC accept failed")?;
        let config = config.clone();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(config, state, stream).await {
                tracing::warn!(target: "focaldesk.automation", error = %err, "IPC connection error");
            }
        });
    }
}

async fn handle_connection(
    config: Arc<AutomationConfig>,
    state: SharedRunState,
    mut stream: UnixStream,
) -> Result<()> {
    let mut input = Vec::new();
    stream
        .read_to_end(&mut input)
        .await
        .context("failed to read automation IPC request")?;

    let response = match serde_json::from_slice::<AutomationIpcRequest>(&input) {
        Ok(AutomationIpcRequest::ListAutomations) => {
            let snapshot = state
                .lock()
                .expect("automation run state lock poisoned")
                .clone();
            let automations = config
                .automations
                .iter()
                .map(|automation| {
                    let run_state = snapshot.get(&automation.name).cloned().unwrap_or_default();
                    AutomationSummary {
                        name: automation.name.clone(),
                        schedule: automation.schedule.clone(),
                        enabled: automation.enabled,
                        last_run_unix: run_state.last_run_unix,
                        last_error: run_state.last_error,
                    }
                })
                .collect();
            AutomationIpcResponse::Automations { automations }
        }
        Ok(AutomationIpcRequest::RunNow { name }) => {
            match config
                .automations
                .iter()
                .find(|automation| automation.name == name)
            {
                Some(automation) => {
                    runner::run_and_record(automation, &state).await;
                    AutomationIpcResponse::RanOk { name }
                }
                None => AutomationIpcResponse::Error {
                    message: format!("no automation named '{name}'"),
                },
            }
        }
        Ok(AutomationIpcRequest::Status) => AutomationIpcResponse::Status {
            automation_count: config.automations.len(),
            config_path: crate::config::config_path().display().to_string(),
        },
        Err(err) => AutomationIpcResponse::Error {
            message: format!("invalid automation IPC request: {err}"),
        },
    };

    let output =
        serde_json::to_vec(&response).context("failed to encode automation IPC response")?;
    stream
        .write_all(&output)
        .await
        .context("failed to write automation IPC response")?;
    stream.shutdown().await.ok();

    Ok(())
}

pub fn send_automation_request(request: &AutomationIpcRequest) -> Result<AutomationIpcResponse> {
    send_automation_request_at(&automation_socket_path(), request)
}

pub fn send_automation_request_at(
    path: impl AsRef<Path>,
    request: &AutomationIpcRequest,
) -> Result<AutomationIpcResponse> {
    let path = path.as_ref();
    let mut stream = StdUnixStream::connect(path).with_context(|| {
        format!(
            "could not connect to automation IPC socket {}",
            path.display()
        )
    })?;
    let json = serde_json::to_vec(request).context("failed to encode automation IPC request")?;

    stream
        .write_all(&json)
        .context("failed to write automation IPC request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish automation IPC request")?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .context("failed to read automation IPC response")?;

    if response.trim().is_empty() {
        bail!("automation IPC returned an empty response");
    }

    serde_json::from_str(&response).context("failed to decode automation IPC response")
}
