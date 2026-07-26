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
use focaldesk_ipc::transport;

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

pub fn automation_socket_path() -> Result<PathBuf> {
    transport::socket_path(AUTOMATION_SOCKET_ENV, AUTOMATION_SOCKET_NAME)
        .map_err(anyhow::Error::msg)
}

pub async fn serve(config: Arc<AutomationConfig>, state: SharedRunState) -> Result<()> {
    let path = automation_socket_path()?;
    let listener = transport::bind_user_socket(&path)
        .with_context(|| format!("failed to bind automation IPC socket {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .context("configure automation IPC listener")?;
    let listener = UnixListener::from_std(listener).context("adopt automation IPC listener")?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("automation IPC accept failed")?;
        if let Err(err) = transport::require_authorized_peer(&stream, transport::AUTOMATION_POLICY)
        {
            tracing::warn!(
                target: "focaldesk.automation",
                error = %err,
                "rejected automation IPC peer"
            );
            continue;
        }
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
    (&mut stream)
        .take(transport::MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .await
        .context("failed to read automation IPC request")?;
    if input.len() as u64 > transport::MAX_REQUEST_BYTES {
        bail!(
            "automation IPC request exceeds {} bytes",
            transport::MAX_REQUEST_BYTES
        );
    }

    let response = match transport::decode_message::<AutomationIpcRequest>(&input) {
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

    let output = transport::encode_message(&response).map_err(anyhow::Error::msg)?;
    stream
        .write_all(&output)
        .await
        .context("failed to write automation IPC response")?;
    stream.shutdown().await.ok();

    Ok(())
}

pub fn send_automation_request(request: &AutomationIpcRequest) -> Result<AutomationIpcResponse> {
    send_automation_request_at(automation_socket_path()?, request)
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
    transport::configure_stream(&stream).context("configure automation IPC connection")?;
    let json = transport::encode_message(request).map_err(anyhow::Error::msg)?;

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

    transport::decode_message(response.as_bytes()).map_err(anyhow::Error::msg)
}
