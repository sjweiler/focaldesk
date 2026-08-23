use anyhow::{Context, Result, bail};
use focaldesk_memory::{MemoryId, MemoryStatus, SearchHit};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::Instrument;

use crate::service::AiService;
use crate::types::{
    AiDaemonStatus, AiStreamEvent, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo,
};
use crate::{AgentActionResponse, AgentRequest, AgentResponse};
use focaldesk_ipc::transport;

pub const AI_SOCKET_NAME: &str = "focaldesk-ai.sock";
pub const AI_SOCKET_ENV: &str = "FOCALDESK_AI_SOCKET";
pub const AI_PROTOCOL_VERSION: u16 = 2;
pub const AI_LEGACY_PROTOCOL_VERSION: u16 = 1;
pub const AI_MAX_REQUEST_BYTES: u64 = 256 * 1024;
pub const AI_MAX_RESPONSE_BYTES: usize = 512 * 1024;
static NEXT_AI_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
// The provider layer permits a chat request to run for 120 seconds. Keep the
// client alive slightly longer so the daemon can return its timeout error (or
// a response completed near the deadline) instead of failing after the shared
// five-second IPC timeout.
const AI_RESPONSE_TIMEOUT: Duration = Duration::from_secs(130);

#[derive(Debug, Serialize, Deserialize)]
struct AiRequestEnvelope<T> {
    ai_protocol_version: u16,
    request_id: String,
    payload: T,
}

#[derive(Debug, Serialize, Deserialize)]
struct AiResponseEnvelope<T> {
    ai_protocol_version: u16,
    request_id: String,
    payload: T,
}

#[derive(Debug, Clone)]
enum AiWireMode {
    Legacy,
    Versioned { request_id: String },
}

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
    ChatStream {
        request: ChatRequest,
    },
    CancelStream {
        request_id: String,
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
    Forget {
        id: MemoryId,
    },
    MemoryStatus,
    ClearMemory,
    RunAgent {
        request: AgentRequest,
    },
    ConfirmAgentAction {
        plan_id: String,
        approved: bool,
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
    Stream {
        event: crate::types::AiStreamEvent,
    },
    Cancellation {
        request_id: String,
        accepted: bool,
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
    Forgotten {
        id: MemoryId,
    },
    MemoryStatus {
        status: MemoryStatus,
    },
    MemoryCleared {
        deleted: usize,
    },
    Agent {
        response: AgentResponse,
    },
    AgentAction {
        response: AgentActionResponse,
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
        .take(AI_MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .await
        .context("failed to read AI IPC request")?;
    if input.len() as u64 > AI_MAX_REQUEST_BYTES {
        bail!("AI IPC request exceeds {AI_MAX_REQUEST_BYTES} bytes");
    }

    let (wire_mode, decoded) = decode_ai_request(&input);
    let request_id = match &wire_mode {
        AiWireMode::Legacy => "legacy".to_string(),
        AiWireMode::Versioned { request_id } => request_id.clone(),
    };
    let span = tracing::info_span!(
        target: "focaldesk.ai",
        "ai_ipc_request",
        request_id = %request_id,
        ai_protocol_version = match &wire_mode {
            AiWireMode::Legacy => AI_LEGACY_PROTOCOL_VERSION,
            AiWireMode::Versioned { .. } => AI_PROTOCOL_VERSION,
        }
    );

    let decoded = match decoded {
        Ok(AiIpcRequest::ChatStream { request }) => {
            let request_id = match &wire_mode {
                AiWireMode::Versioned { request_id } => request_id.clone(),
                AiWireMode::Legacy => {
                    let response = AiIpcResponse::Error {
                        message: "streaming chat requires AI IPC protocol v2".into(),
                    };
                    let output =
                        encode_ai_response(&response, &wire_mode).map_err(anyhow::Error::msg)?;
                    stream.write_all(&output).await?;
                    stream.shutdown().await.ok();
                    return Ok(());
                }
            };
            return handle_stream_connection(service, stream, wire_mode, request_id, request, span)
                .await;
        }
        other => other,
    };

    let response = async move {
        tracing::info!(target: "focaldesk.ai", "AI IPC request dispatching");
        match decoded {
            Ok(AiIpcRequest::ListProviders) => AiIpcResponse::Providers {
                default_provider: service.default_provider().to_string(),
                providers: service.providers(),
            },
            Ok(AiIpcRequest::ListModels { provider }) => {
                match service.provider_models(&provider).await {
                    Ok(models) => AiIpcResponse::Models { provider, models },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::Chat { request }) => match service.chat(request).await {
                Ok(response) => AiIpcResponse::Chat { response },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::CancelStream { request_id }) => {
                match service.cancel_stream(&request_id) {
                    Ok(accepted) => AiIpcResponse::Cancellation {
                        request_id,
                        accepted,
                    },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ChatStream { .. }) => AiIpcResponse::Error {
                message: "streaming request dispatch invariant failed".into(),
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
            Ok(AiIpcRequest::Forget { id }) => match service.forget(id).await {
                Ok(()) => AiIpcResponse::Forgotten { id },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::MemoryStatus) => match service.memory_status().await {
                Ok(status) => AiIpcResponse::MemoryStatus { status },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ClearMemory) => match service.clear_memory().await {
                Ok(deleted) => AiIpcResponse::MemoryCleared { deleted },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RunAgent { request }) => match service.run_agent(request).await {
                Ok(response) => AiIpcResponse::Agent { response },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ConfirmAgentAction { plan_id, approved }) => {
                match service.confirm_agent_action(plan_id, approved).await {
                    Ok(response) => AiIpcResponse::AgentAction { response },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Err(err) => AiIpcResponse::Error {
                message: format!("invalid AI IPC request: {err}"),
            },
        }
    }
    .instrument(span)
    .await;

    let output = encode_ai_response(&response, &wire_mode).map_err(anyhow::Error::msg)?;
    stream
        .write_all(&output)
        .await
        .context("failed to write AI IPC response")?;
    stream.shutdown().await.ok();

    Ok(())
}

async fn handle_stream_connection(
    service: Arc<AiService>,
    mut stream: UnixStream,
    wire_mode: AiWireMode,
    request_id: String,
    request: ChatRequest,
    span: tracing::Span,
) -> Result<()> {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
    let service_for_task = service.clone();
    let request_id_for_task = request_id.clone();
    let event_tx_for_error = event_tx.clone();
    let task = tokio::spawn(
        async move {
            if let Err(err) = service_for_task
                .chat_stream(request_id_for_task.clone(), request, event_tx)
                .await
            {
                let _ = event_tx_for_error
                    .send(crate::types::AiStreamEvent::Failed {
                        request_id: request_id_for_task,
                        message: err.to_string(),
                    })
                    .await;
            }
        }
        .instrument(span),
    );

    while let Some(event) = event_rx.recv().await {
        let terminal = matches!(
            &event,
            crate::types::AiStreamEvent::Completed { .. }
                | crate::types::AiStreamEvent::Failed { .. }
                | crate::types::AiStreamEvent::Cancelled { .. }
        );
        let response = AiIpcResponse::Stream { event };
        let mut output = encode_ai_response(&response, &wire_mode).map_err(anyhow::Error::msg)?;
        output.push(b'\n');
        if let Err(err) = stream.write_all(&output).await {
            let _ = service.cancel_stream(&request_id);
            task.abort();
            return Err(err).context("write AI stream event");
        }
        if terminal {
            break;
        }
    }
    let _ = task.await;
    stream.shutdown().await.ok();
    Ok(())
}

pub fn send_ai_request(request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = ai_socket_path()?;
    send_ai_request_at(&path, request)
}

/// Stream a chat response from the AI daemon, invoking `on_event` for each
/// framed event. Every event includes the request id, so a `Started` handler
/// can hand it to another thread for use with [`cancel_ai_stream`].
pub fn stream_ai_chat(
    request: ChatRequest,
    on_event: impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<String> {
    let path = ai_socket_path()?;
    stream_ai_chat_at(path, request, on_event)
}

pub fn stream_ai_chat_at(
    path: impl AsRef<Path>,
    request: ChatRequest,
    mut on_event: impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<String> {
    let path = path.as_ref();
    let request_id = next_request_id();
    let mut stream = StdUnixStream::connect(path)
        .with_context(|| format!("could not connect to AI IPC socket {}", path.display()))?;
    configure_ai_stream(&stream)?;
    let encoded = encode_ai_request(&AiIpcRequest::ChatStream { request }, &request_id)
        .map_err(anyhow::Error::msg)?;
    stream
        .write_all(&encoded)
        .context("failed to write AI stream request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish AI stream request")?;

    let mut reader = BufReader::new(stream);
    read_ai_stream_events(&mut reader, &request_id, &mut on_event)?;
    Ok(request_id)
}

fn read_ai_stream_events(
    reader: &mut impl BufRead,
    request_id: &str,
    mut on_event: impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    loop {
        let mut frame = Vec::new();
        let mut bounded_reader = reader.take((AI_MAX_RESPONSE_BYTES + 2) as u64);
        let bytes = bounded_reader
            .read_until(b'\n', &mut frame)
            .context("failed to read AI stream event")?;
        if bytes == 0 {
            bail!("AI stream closed before a terminal event");
        }
        if frame.len() > AI_MAX_RESPONSE_BYTES + 1 || frame.last() != Some(&b'\n') {
            bail!("AI stream event exceeds {AI_MAX_RESPONSE_BYTES} bytes");
        }
        frame.pop();
        let (response, mode) = decode_ai_response(&frame, Some(&request_id))?;
        if matches!(mode, AiWireMode::Legacy) {
            bail!("AI streaming requires daemon protocol version {AI_PROTOCOL_VERSION}");
        }
        match response {
            AiIpcResponse::Stream { event } => {
                let event_request_id = match &event {
                    AiStreamEvent::Started { request_id, .. }
                    | AiStreamEvent::Delta { request_id, .. }
                    | AiStreamEvent::Completed { request_id, .. }
                    | AiStreamEvent::Failed { request_id, .. }
                    | AiStreamEvent::Cancelled { request_id } => request_id,
                };
                if event_request_id != request_id {
                    bail!(
                        "AI stream event request id mismatch: expected {request_id}, received {event_request_id}"
                    );
                }
                let terminal = matches!(
                    &event,
                    AiStreamEvent::Completed { .. }
                        | AiStreamEvent::Failed { .. }
                        | AiStreamEvent::Cancelled { .. }
                );
                on_event(event)?;
                if terminal {
                    return Ok(());
                }
            }
            AiIpcResponse::Error { message } => bail!(message),
            other => bail!("unexpected AI stream response: {other:?}"),
        }
    }
}

pub fn cancel_ai_stream(request_id: &str) -> Result<bool> {
    let path = ai_socket_path()?;
    cancel_ai_stream_at(path, request_id)
}

pub fn cancel_ai_stream_at(path: impl AsRef<Path>, request_id: &str) -> Result<bool> {
    if !valid_request_id(request_id) {
        bail!("invalid AI stream request id");
    }
    match send_ai_request_at(
        path,
        &AiIpcRequest::CancelStream {
            request_id: request_id.to_string(),
        },
    )? {
        AiIpcResponse::Cancellation { accepted, .. } => Ok(accepted),
        AiIpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected AI cancellation response: {other:?}"),
    }
}

pub fn ai_socket_path() -> Result<PathBuf> {
    transport::socket_path(AI_SOCKET_ENV, AI_SOCKET_NAME).map_err(anyhow::Error::msg)
}

fn next_request_id() -> String {
    let sequence = NEXT_AI_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{sequence}", std::process::id())
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= 64
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn encode_ai_request(request: &AiIpcRequest, request_id: &str) -> Result<Vec<u8>, String> {
    if !valid_request_id(request_id) {
        return Err("invalid AI IPC request id".to_string());
    }
    let output = transport::encode_message(&AiRequestEnvelope {
        ai_protocol_version: AI_PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        payload: request,
    })?;
    if output.len() as u64 > AI_MAX_REQUEST_BYTES {
        return Err(format!(
            "AI IPC request exceeds {AI_MAX_REQUEST_BYTES} bytes"
        ));
    }
    Ok(output)
}

fn decode_ai_request(bytes: &[u8]) -> (AiWireMode, Result<AiIpcRequest, String>) {
    let value = match transport::decode_message::<serde_json::Value>(bytes) {
        Ok(value) => value,
        Err(err) => return (AiWireMode::Legacy, Err(err)),
    };
    if value.get("ai_protocol_version").is_none() {
        return (
            AiWireMode::Legacy,
            serde_json::from_value(value).map_err(|err| err.to_string()),
        );
    }

    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("invalid")
        .to_string();
    let mode = AiWireMode::Versioned {
        request_id: request_id.clone(),
    };
    if !valid_request_id(&request_id) {
        return (mode, Err("invalid AI IPC request id".to_string()));
    }
    let version = value
        .get("ai_protocol_version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(AI_PROTOCOL_VERSION as u64) {
        return (
            mode,
            Err(format!(
                "unsupported AI protocol version {}; supported version is {}",
                version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "invalid".to_string()),
                AI_PROTOCOL_VERSION
            )),
        );
    }
    let payload = value
        .get("payload")
        .cloned()
        .ok_or_else(|| "AI IPC envelope is missing payload".to_string())
        .and_then(|payload| serde_json::from_value(payload).map_err(|err| err.to_string()));
    (mode, payload)
}

fn encode_ai_response(response: &AiIpcResponse, mode: &AiWireMode) -> Result<Vec<u8>, String> {
    let output = match mode {
        AiWireMode::Legacy => transport::encode_message(response),
        AiWireMode::Versioned { request_id } => transport::encode_message(&AiResponseEnvelope {
            ai_protocol_version: AI_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            payload: response,
        }),
    }?;
    if output.len() > AI_MAX_RESPONSE_BYTES {
        let bounded_error = AiIpcResponse::Error {
            message: format!("AI IPC response exceeds {AI_MAX_RESPONSE_BYTES} bytes"),
        };
        return match mode {
            AiWireMode::Legacy => transport::encode_message(&bounded_error),
            AiWireMode::Versioned { request_id } => {
                transport::encode_message(&AiResponseEnvelope {
                    ai_protocol_version: AI_PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    payload: &bounded_error,
                })
            }
        };
    }
    Ok(output)
}

fn decode_ai_response(
    bytes: &[u8],
    expected_request_id: Option<&str>,
) -> Result<(AiIpcResponse, AiWireMode)> {
    let value =
        transport::decode_message::<serde_json::Value>(bytes).map_err(anyhow::Error::msg)?;
    if value.get("ai_protocol_version").is_none() {
        let response = serde_json::from_value(value).context("decode legacy AI IPC response")?;
        return Ok((response, AiWireMode::Legacy));
    }
    let envelope: AiResponseEnvelope<AiIpcResponse> =
        serde_json::from_value(value).context("decode versioned AI IPC response")?;
    if envelope.ai_protocol_version != AI_PROTOCOL_VERSION {
        bail!(
            "unsupported AI response protocol version {}; supported version is {}",
            envelope.ai_protocol_version,
            AI_PROTOCOL_VERSION
        );
    }
    if let Some(expected) = expected_request_id
        && envelope.request_id != expected
    {
        bail!(
            "AI IPC response request id mismatch: expected {expected}, received {}",
            envelope.request_id
        );
    }
    let mode = AiWireMode::Versioned {
        request_id: envelope.request_id,
    };
    Ok((envelope.payload, mode))
}

fn configure_ai_stream(stream: &StdUnixStream) -> Result<()> {
    transport::configure_stream(stream).context("configure AI IPC connection")?;
    stream
        .set_read_timeout(Some(AI_RESPONSE_TIMEOUT))
        .context("configure AI IPC response timeout")
}

pub fn send_ai_request_at(path: impl AsRef<Path>, request: &AiIpcRequest) -> Result<AiIpcResponse> {
    let path = path.as_ref();
    let request_id = next_request_id();
    let (response, mode) = send_ai_request_at_mode(path, request, Some(&request_id))?;
    if matches!(mode, AiWireMode::Legacy)
        && matches!(&response, AiIpcResponse::Error { message } if message.contains("invalid AI IPC request"))
    {
        return send_ai_request_at_mode(path, request, None).map(|(response, _)| response);
    }
    Ok(response)
}

fn send_ai_request_at_mode(
    path: &Path,
    request: &AiIpcRequest,
    request_id: Option<&str>,
) -> Result<(AiIpcResponse, AiWireMode)> {
    let mut stream = StdUnixStream::connect(path)
        .with_context(|| format!("could not connect to AI IPC socket {}", path.display()))?;
    configure_ai_stream(&stream)?;
    let json = match request_id {
        Some(request_id) => encode_ai_request(request, request_id),
        None => transport::encode_message(request),
    }
    .map_err(anyhow::Error::msg)?;
    if json.len() as u64 > AI_MAX_REQUEST_BYTES {
        bail!("AI IPC request exceeds {AI_MAX_REQUEST_BYTES} bytes");
    }

    stream
        .write_all(&json)
        .context("failed to write AI IPC request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish AI IPC request")?;

    read_ai_response(&mut stream, AI_RESPONSE_TIMEOUT, request_id)
}

fn read_ai_response(
    reader: &mut impl Read,
    response_timeout: Duration,
    expected_request_id: Option<&str>,
) -> Result<(AiIpcResponse, AiWireMode)> {
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

    if response.len() > AI_MAX_RESPONSE_BYTES {
        bail!("AI IPC response exceeds {AI_MAX_RESPONSE_BYTES} bytes");
    }
    decode_ai_response(&response, expected_request_id)
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
    fn stream_client_decodes_multiple_framed_events() {
        let request_id = "test-stream-1";
        let mode = AiWireMode::Versioned {
            request_id: request_id.into(),
        };
        let response = ChatResponse {
            provider: "test".into(),
            model: Some("model".into()),
            content: "hello".into(),
            usage: None,
        };
        let mut bytes = Vec::new();
        for event in [
            AiStreamEvent::Started {
                request_id: request_id.into(),
                provider: "test".into(),
                model: Some("model".into()),
            },
            AiStreamEvent::Delta {
                request_id: request_id.into(),
                content: "hello".into(),
            },
            AiStreamEvent::Completed {
                request_id: request_id.into(),
                response,
            },
        ] {
            bytes.extend(encode_ai_response(&AiIpcResponse::Stream { event }, &mode).unwrap());
            bytes.push(b'\n');
        }

        let mut events = Vec::new();
        read_ai_stream_events(&mut Cursor::new(bytes), request_id, |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();

        assert_eq!(events.len(), 3);
        assert!(matches!(events[1], AiStreamEvent::Delta { .. }));
        assert!(matches!(events[2], AiStreamEvent::Completed { .. }));
    }

    #[test]
    fn stream_client_rejects_disconnect_without_terminal_event() {
        let request_id = "test-stream-2";
        let mode = AiWireMode::Versioned {
            request_id: request_id.into(),
        };
        let mut frame = encode_ai_response(
            &AiIpcResponse::Stream {
                event: AiStreamEvent::Delta {
                    request_id: request_id.into(),
                    content: "partial".into(),
                },
            },
            &mode,
        )
        .unwrap();
        frame.push(b'\n');

        let error =
            read_ai_stream_events(&mut Cursor::new(frame), request_id, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("terminal event"));
    }

    #[test]
    fn stream_client_rejects_event_request_id_mismatch() {
        let request_id = "test-stream-3";
        let mode = AiWireMode::Versioned {
            request_id: request_id.into(),
        };
        let mut frame = encode_ai_response(
            &AiIpcResponse::Stream {
                event: AiStreamEvent::Cancelled {
                    request_id: "different-id".into(),
                },
            },
            &mode,
        )
        .unwrap();
        frame.push(b'\n');

        let error =
            read_ai_stream_events(&mut Cursor::new(frame), request_id, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("event request id mismatch"));
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
                usage: None,
            },
        })
        .unwrap();
        let mut reader = DelayedReader {
            inner: Cursor::new(encoded),
            delay: Some(Duration::from_millis(100)),
        };
        let (response, mode) = read_ai_response(&mut reader, Duration::from_secs(1), None).unwrap();
        assert!(matches!(mode, AiWireMode::Legacy));

        match response {
            AiIpcResponse::Chat { response } => {
                assert_eq!(response.content, "delayed response received");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn current_protocol_round_trips_request_id_and_payload() {
        let encoded = encode_ai_request(&AiIpcRequest::Status, "test-42").unwrap();
        let (mode, request) = decode_ai_request(&encoded);
        assert!(matches!(
            mode,
            AiWireMode::Versioned { request_id } if request_id == "test-42"
        ));
        assert!(matches!(request.unwrap(), AiIpcRequest::Status));

        let response = encode_ai_response(
            &AiIpcResponse::Error {
                message: "test".into(),
            },
            &AiWireMode::Versioned {
                request_id: "test-42".into(),
            },
        )
        .unwrap();
        let (_, response_mode) = decode_ai_response(&response, Some("test-42")).unwrap();
        assert!(matches!(response_mode, AiWireMode::Versioned { .. }));
    }

    #[test]
    fn legacy_bare_payload_remains_accepted() {
        let encoded = transport::encode_message(&AiIpcRequest::Status).unwrap();
        let (mode, request) = decode_ai_request(&encoded);
        assert!(matches!(mode, AiWireMode::Legacy));
        assert!(matches!(request.unwrap(), AiIpcRequest::Status));
    }

    #[test]
    fn unsupported_ai_protocol_version_is_explicitly_rejected() {
        let encoded = transport::encode_message(&serde_json::json!({
            "ai_protocol_version": 99,
            "request_id": "test-99",
            "payload": {"type": "Status"}
        }))
        .unwrap();
        let (mode, error) = decode_ai_request(&encoded);
        assert!(matches!(mode, AiWireMode::Versioned { .. }));
        assert!(
            error
                .unwrap_err()
                .contains("unsupported AI protocol version 99")
        );
    }

    #[test]
    fn response_request_id_mismatch_is_rejected() {
        let encoded = encode_ai_response(
            &AiIpcResponse::Error {
                message: "test".into(),
            },
            &AiWireMode::Versioned {
                request_id: "actual-1".into(),
            },
        )
        .unwrap();
        let error = decode_ai_response(&encoded, Some("expected-1")).unwrap_err();
        assert!(error.to_string().contains("request id mismatch"));
    }

    #[test]
    fn ai_specific_payload_limits_are_enforced() {
        let oversized = AiIpcRequest::Remember {
            text: "x".repeat(AI_MAX_REQUEST_BYTES as usize),
            metadata: serde_json::Value::Null,
        };
        assert!(
            encode_ai_request(&oversized, "large-1")
                .unwrap_err()
                .contains("request exceeds")
        );

        let oversized_response = AiIpcResponse::Error {
            message: "x".repeat(AI_MAX_RESPONSE_BYTES),
        };
        let encoded = encode_ai_response(
            &oversized_response,
            &AiWireMode::Versioned {
                request_id: "large-1".into(),
            },
        )
        .unwrap();
        assert!(encoded.len() < AI_MAX_RESPONSE_BYTES);
        let (response, _) = decode_ai_response(&encoded, Some("large-1")).unwrap();
        assert!(matches!(
            response,
            AiIpcResponse::Error { message } if message.contains("response exceeds")
        ));
    }
}
