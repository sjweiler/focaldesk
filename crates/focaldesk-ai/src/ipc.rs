use anyhow::{Context, Result, bail};
use focaldesk_memory::{MemoryId, SearchHit};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::service::AiService;
use crate::types::{AiDaemonStatus, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo};
use focaldesk_ipc::transport;

pub const AI_SOCKET_NAME: &str = "focaldesk-ai.sock";
pub const AI_SOCKET_ENV: &str = "FOCALDESK_AI_SOCKET";
// The provider layer permits a chat request to run for 120 seconds. Keep the
// client alive slightly longer so the daemon can return its timeout error (or
// a response completed near the deadline) instead of failing after the shared
// five-second IPC timeout.
const AI_RESPONSE_TIMEOUT: Duration = Duration::from_secs(130);

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

fn configure_ai_stream(stream: &StdUnixStream) -> Result<()> {
    transport::configure_stream(stream).context("configure AI IPC connection")?;
    stream
        .set_read_timeout(Some(AI_RESPONSE_TIMEOUT))
        .context("configure AI IPC response timeout")
}

pub fn send_ai_request_at(path: impl AsRef<Path>, request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = path.as_ref();
    let mut stream = StdUnixStream::connect(path)
        .with_context(|| format!("could not connect to AI IPC socket {}", path.display()))?;
    configure_ai_stream(&stream)?;
    let json = transport::encode_message(request).map_err(anyhow::Error::msg)?;

    stream
        .write_all(&json)
        .context("failed to write AI IPC request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish AI IPC request")?;

    read_ai_response(&mut stream, AI_RESPONSE_TIMEOUT)
}

fn read_ai_response(reader: &mut impl Read, response_timeout: Duration) -> Result<AiIpcResponse> {
    let mut response = Vec::new();
    if let Err(err) = reader.read_to_end(&mut response) {
        if matches!(
            err.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            bail!(
                "AI daemon did not respond within {} seconds",
                response_timeout.as_secs_f64()
            );
        }
        // A peer can report ECONNRESET while closing a Unix stream after its
        // final write. If a complete response was received before that close,
        // it is still valid and should not be discarded as a transport error.
        // This is particularly common when the daemon is restarted while a
        // request is in flight.
        if response.is_empty() {
            return Err(err).context("failed to read AI IPC response");
        }
    }

    if response.iter().all(u8::is_ascii_whitespace) {
        bail!("AI IPC returned an empty response");
    }

    transport::decode_message(&response).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatResponse;
    use std::io::Cursor;

    #[test]
    fn ai_response_timeout_outlasts_provider_timeout() {
        assert!(AI_RESPONSE_TIMEOUT > Duration::from_secs(120));

        let (client, _server) = StdUnixStream::pair().unwrap();
        if let Err(err) = configure_ai_stream(&client) {
            if err.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
            }) {
                // Some restricted test sandboxes prohibit socket timeout
                // configuration. The duration relationship above is still
                // platform-independent.
                return;
            }
            panic!("configure AI stream: {err:#}");
        }
        assert_eq!(client.read_timeout().unwrap(), Some(AI_RESPONSE_TIMEOUT));
    }

    #[test]
    fn reads_and_decodes_a_delayed_ai_response() {
        struct DelayedReader {
            inner: Cursor<Vec<u8>>,
            delay: Option<Duration>,
        }

        impl Read for DelayedReader {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if let Some(delay) = self.delay.take() {
                    std::thread::sleep(delay);
                }
                std::io::Read::read(&mut self.inner, output)
            }
        }

        let encoded = transport::encode_message(&AiIpcResponse::Chat {
            response: ChatResponse {
                provider: "test-provider".to_string(),
                model: Some("test-model".to_string()),
                content: "delayed response received".to_string(),
            },
        })
        .unwrap();
        let mut reader = DelayedReader {
            inner: Cursor::new(encoded),
            delay: Some(Duration::from_millis(100)),
        };
        let response = read_ai_response(&mut reader, Duration::from_secs(1)).unwrap();

        match response {
            AiIpcResponse::Chat { response } => {
                assert_eq!(response.content, "delayed response received");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
