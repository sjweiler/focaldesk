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
    let path = ai_socket_path();
    serve_ai_ipc_at(service, &path).await
}

pub async fn serve_ai_ipc_at(service: Arc<AiService>, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind AI IPC socket {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    loop {
        let (stream, _) = listener.accept().await.context("AI IPC accept failed")?;
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
    stream
        .read_to_end(&mut input)
        .await
        .context("failed to read AI IPC request")?;

    let response = match serde_json::from_slice::<AiIpcRequest>(&input) {
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

    let output = serde_json::to_vec(&response).context("failed to encode AI IPC response")?;
    stream
        .write_all(&output)
        .await
        .context("failed to write AI IPC response")?;
    stream.shutdown().await.ok();

    Ok(())
}

pub fn send_ai_request(request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = ai_socket_path();
    send_ai_request_at(&path, request)
}

pub fn ai_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os(AI_SOCKET_ENV) {
        return PathBuf::from(path);
    }

    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(AI_SOCKET_NAME)
}

pub fn send_ai_request_at(path: impl AsRef<Path>, request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = path.as_ref();
    let mut stream = StdUnixStream::connect(path)
        .with_context(|| format!("could not connect to AI IPC socket {}", path.display()))?;
    let json = serde_json::to_vec(request).context("failed to encode AI IPC request")?;

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

    serde_json::from_str(&response).context("failed to decode AI IPC response")
}
