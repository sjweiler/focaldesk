use anyhow::{Context, Result, bail};
use focaldesk_memory::{MemoryId, SearchHit};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::service::AiService;
use crate::types::{AiDaemonStatus, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo};
use focaldesk_ipc::transport;

pub const AI_SOCKET_NAME: &str = "focaldesk-ai.sock";
pub const AI_SOCKET_ENV: &str = "FOCALDESK_AI_SOCKET";

fn default_recall_top_k() -> usize {
    5
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AiIpcRequest {
    ListProviders,
    ListModels {
        provider: String,
    },
    Chat {
        request: ChatRequest,
    },
    Status,
    Remember {
        text: String,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    Recall {
        query: String,
        #[serde(default = "default_recall_top_k")]
        top_k: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AiIpcResponse {
    Providers {
        default_provider: String,
        providers: Vec<ProviderInfo>,
    },
    Models {
        provider: String,
        models: Vec<ProviderModelInfo>,
    },
    Chat {
        response: ChatResponse,
    },
    Status {
        status: AiDaemonStatus,
    },
    Remembered {
        id: MemoryId,
    },
    Recalled {
        hits: Vec<SearchHit>,
    },
    Error {
        message: String,
    },
}

pub async fn serve_ai_ipc(service: Arc<AiService>) -> Result<()> {
    let path = ai_socket_path()?;
    serve_ai_ipc_at_inner(service, &path, true).await
}

/// Serve an isolated same-user endpoint for integration tests.
///
/// Production services must use [`serve_ai_ipc`], which also enforces the
/// endpoint-specific application policy.
pub async fn serve_ai_ipc_at(service: Arc<AiService>, path: impl AsRef<Path>) -> Result<()> {
    serve_ai_ipc_at_inner(service, path.as_ref(), false).await
}

async fn serve_ai_ipc_at_inner(
    service: Arc<AiService>,
    path: &Path,
    enforce_application_policy: bool,
) -> Result<()> {
    let listener = transport::bind_user_socket(path)
        .with_context(|| format!("failed to bind AI IPC socket {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .context("configure AI IPC listener")?;
    let listener = UnixListener::from_std(listener).context("adopt AI IPC listener")?;

    loop {
        let (stream, _) = listener.accept().await.context("AI IPC accept failed")?;
        let authorization = if enforce_application_policy {
            transport::require_authorized_peer(&stream, transport::AI_POLICY).map(|_| ())
        } else {
            transport::require_same_user(&stream)
        };
        if let Err(err) = authorization {
            tracing::warn!(target: "focaldesk.ai", error = %err, "rejected AI IPC peer");
            continue;
        }
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(service, stream).await {
                eprintln!("AI IPC connection error: {err:?}");
            }
        });
    }
}

async fn handle_connection(service: Arc<AiService>, mut stream: UnixStream) -> Result<()> {
    let mut input = Vec::new();
    (&mut stream)
        .take(transport::MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .await
        .context("failed to read AI IPC request")?;
    if input.len() as u64 > transport::MAX_REQUEST_BYTES {
        bail!(
            "AI IPC request exceeds {} bytes",
            transport::MAX_REQUEST_BYTES
        );
    }

    let response = match transport::decode_message::<AiIpcRequest>(&input) {
        Ok(AiIpcRequest::ListProviders) => AiIpcResponse::Providers {
            default_provider: service.default_provider().to_string(),
            providers: service.providers(),
        },
        Ok(AiIpcRequest::ListModels { provider }) => match service.provider_models(&provider).await
        {
            Ok(models) => AiIpcResponse::Models { provider, models },
            Err(err) => AiIpcResponse::Error {
                message: err.to_string(),
            },
        },
        Ok(AiIpcRequest::Chat { request }) => match service.chat(request).await {
            Ok(response) => AiIpcResponse::Chat { response },
            Err(err) => AiIpcResponse::Error {
                message: err.to_string(),
            },
        },
        Ok(AiIpcRequest::Status) => AiIpcResponse::Status {
            status: service.status(),
        },
        Ok(AiIpcRequest::Remember { text, metadata }) => {
            match service.remember(text, metadata).await {
                Ok(id) => AiIpcResponse::Remembered { id },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        Ok(AiIpcRequest::Recall { query, top_k }) => match service.recall(query, top_k).await {
            Ok(hits) => AiIpcResponse::Recalled { hits },
            Err(err) => AiIpcResponse::Error {
                message: err.to_string(),
            },
        },
        Err(err) => AiIpcResponse::Error {
            message: format!("invalid AI IPC request: {err}"),
        },
    };

    let output = transport::encode_message(&response).map_err(anyhow::Error::msg)?;
    stream
        .write_all(&output)
        .await
        .context("failed to write AI IPC response")?;
    stream.shutdown().await.ok();

    Ok(())
}

pub fn send_ai_request(request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = ai_socket_path()?;
    send_ai_request_at(&path, request)
}

pub fn ai_socket_path() -> Result<PathBuf> {
    transport::socket_path(AI_SOCKET_ENV, AI_SOCKET_NAME).map_err(anyhow::Error::msg)
}

pub fn send_ai_request_at(path: impl AsRef<Path>, request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = path.as_ref();
    let mut stream = StdUnixStream::connect(path)
        .with_context(|| format!("could not connect to AI IPC socket {}", path.display()))?;
    transport::configure_stream(&stream).context("configure AI IPC connection")?;
    let json = transport::encode_message(request).map_err(anyhow::Error::msg)?;

    stream
        .write_all(&json)
        .context("failed to write AI IPC request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish AI IPC request")?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .context("failed to read AI IPC response")?;

    if response.trim().is_empty() {
        bail!("AI IPC returned an empty response");
    }

    transport::decode_message(response.as_bytes()).map_err(anyhow::Error::msg)
}
