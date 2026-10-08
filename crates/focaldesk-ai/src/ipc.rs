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
    RetrievalEvalCase, RetrievalEvalReport,
};
use crate::{
    AgentActionResponse, AgentControlStatus, AgentDefinition, AgentDryRunReport, AgentRequest,
    AgentResponse, AgentRunEvent, AgentRunState, AgentRunStatus, AgentTriggerKind,
};
use focaldesk_ipc::transport;

pub const AI_SOCKET_NAME: &str = "focaldesk-ai.sock";
pub const AI_SOCKET_ENV: &str = "FOCALDESK_AI_SOCKET";
pub const AI_PROTOCOL_VERSION: u16 = 2;
pub const AI_LEGACY_PROTOCOL_VERSION: u16 = 1;
// Signed .fai bundles are bounded to 2 MiB before transport. Leave envelope
// headroom while retaining a strict daemon-side request ceiling.
pub const AI_MAX_REQUEST_BYTES: u64 = 3 * 1024 * 1024;
// Source listings for large repositories can contain thousands of paths and
// hashes. Keep responses bounded, but leave enough room for the documented
// 10,000-file directory-ingestion ceiling.
pub const AI_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
static NEXT_AI_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
// The provider layer permits a chat request to run for 120 seconds. Keep the
// client alive slightly longer so the daemon can return its timeout error (or
// a response completed near the deadline) instead of failing after the shared
// five-second IPC timeout.
const AI_RESPONSE_TIMEOUT: Duration = Duration::from_secs(130);
// Directory ingestion can legitimately take many minutes for a large source
// tree because each changed document must be extracted, chunked, and embedded.
// Keep a finite upper bound so a wedged daemon is still eventually reported,
// but do not apply the provider/chat deadline to local indexing work.
const AI_INGEST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

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

fn default_mission_control_limit() -> usize {
    100
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
    IngestDocument {
        path: PathBuf,
    },
    IngestDirectory {
        path: PathBuf,
        #[serde(default)]
        recursive: bool,
    },
    ListIndexedDocuments,
    RemoveIndexedDocument {
        source: String,
    },
    EvaluateRetrieval {
        cases: Vec<RetrievalEvalCase>,
        #[serde(default = "default_recall_top_k")]
        top_k: usize,
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
    StartAgent {
        request: AgentRequest,
    },
    RetryAgentRun {
        run_id: String,
    },
    FireAgentTrigger {
        agent_id: String,
        trigger_id: String,
    },
    DispatchAgentEvent {
        kind: AgentTriggerKind,
        value: String,
    },
    GetAgentTriggerState,
    GetMissionControl {
        #[serde(default)]
        query: Option<String>,
        #[serde(default = "default_mission_control_limit")]
        limit: usize,
    },
    ActivateMissionControlPause,
    EvaluateScenario {
        fixture: Box<crate::ScenarioFixture>,
    },
    CaptureScenario {
        name: String,
        #[serde(default)]
        query: Option<String>,
        #[serde(default = "default_mission_control_limit")]
        limit: usize,
    },
    InspectPackage {
        bundle: Box<crate::FaiBundle>,
    },
    GeneratePackageSigner {
        signer_id: String,
    },
    BuildPackageProject {
        project: Box<crate::FaiForgeProject>,
    },
    TrustPackageSigner {
        signer: crate::FaiSigner,
    },
    StagePackage {
        bundle: Box<crate::FaiBundle>,
    },
    ActivatePackage {
        package_id: String,
    },
    RollbackPackage {
        package_id: String,
    },
    ListPackages,
    SetAgentTriggersSuspended {
        suspended: bool,
    },
    GetAgentRun {
        run_id: String,
    },
    /// Long-polls until a newer event is available or the run stops.
    WatchAgentRun {
        run_id: String,
        #[serde(default)]
        after_sequence: u64,
    },
    ListAgentRuns,
    ListAgents,
    ReloadAgents,
    InstallAgent {
        definition: Box<AgentDefinition>,
        #[serde(default)]
        overwrite: bool,
    },
    GetAgentControlStatuses,
    SetAgentEnabled {
        agent_id: String,
        enabled: bool,
    },
    RollbackAgent {
        agent_id: String,
    },
    DryRunAgent {
        request: AgentRequest,
    },
    ListWorkflows,
    StartWorkflow {
        workflow_id: String,
    },
    ListWorkflowRuns,
    GetWorkflowRun {
        run_id: String,
    },
    SetWorkflowPaused {
        run_id: String,
        paused: bool,
    },
    CancelWorkflow {
        run_id: String,
    },
    RetryWorkflow {
        run_id: String,
    },
    PreviewCapabilities {
        agent_id: String,
        #[serde(default)]
        ceiling: Option<crate::CapabilityPolicy>,
    },
    ListCapabilityLeases,
    RevokeCapabilityLease {
        lease_id: String,
    },
    PublishContext {
        kind: crate::ContextKind,
        provenance: String,
        sensitivity: crate::ContextSensitivity,
        payload: serde_json::Value,
        ttl_seconds: u64,
    },
    GrantContext {
        agent_id: String,
        kinds: Vec<crate::ContextKind>,
        ttl_seconds: u64,
    },
    GetContextState,
    RevokeContextGrant {
        grant_id: String,
    },
    ClearContext,
    PublishSuggestion {
        agent_id: String,
        title: String,
        body: String,
        ttl_seconds: u64,
    },
    DismissSuggestion {
        suggestion_id: String,
    },
    RouteIntent {
        text: String,
    },
    GetRoutineState,
    DispatchRoutineEvent {
        event: crate::RoutineEvent,
    },
    SimulateRoutineEvent {
        event: crate::RoutineEvent,
    },
    SetRoutinesSuspended {
        suspended: bool,
    },
    DismissRoutineSuggestion {
        suggestion_id: String,
    },
    PromoteRoutineSuggestion {
        suggestion_id: String,
    },
    GetEventFabricState,
    ConfigureEventSource {
        policy: crate::EventSourcePolicy,
    },
    SetEventFabricConnected {
        connected: bool,
    },
    ListConnectors,
    InstallConnector {
        manifest: crate::ConnectorManifest,
        #[serde(default)]
        overwrite: bool,
    },
    SetConnectorEnabled {
        connector_id: String,
        enabled: bool,
        #[serde(default)]
        network_allowed: bool,
    },
    RollbackConnector {
        connector_id: String,
    },
    PublishConnectorEvent {
        request: crate::ConnectorEventRequest,
    },
    PublishManagedConnectorEvent {
        connector_id: String,
        source: crate::EventSource,
        payload: serde_json::Value,
    },
    SimulateEvent {
        source: crate::EventSource,
        producer: String,
        payload: serde_json::Value,
    },
    ReplayEventSimulation {
        event_id: String,
    },
    ClearEventJournal,
    CancelAgentRun {
        run_id: String,
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
    DocumentIngested {
        result: crate::types::DocumentIngestResult,
    },
    DirectoryIngested {
        result: crate::types::DirectoryIngestResult,
    },
    IndexedDocuments {
        documents: Vec<focaldesk_memory::IndexedDocument>,
    },
    IndexedDocumentRemoved {
        source: String,
        removed: bool,
    },
    RetrievalEvaluated {
        report: RetrievalEvalReport,
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
    AgentStarted {
        run_id: String,
    },
    AgentTriggersStarted {
        run_ids: Vec<String>,
    },
    AgentTriggerState {
        suspended: bool,
    },
    MissionControlState {
        state: crate::MissionControlSnapshot,
    },
    ScenarioEvaluated {
        report: crate::ScenarioReport,
    },
    ScenarioCaptured {
        fixture: crate::ScenarioFixture,
    },
    PackageInspected {
        inspection: crate::FaiPackageInspection,
    },
    PackageSignerGenerated {
        signer: crate::FaiSigner,
    },
    PackageBuilt {
        bundle: Box<crate::FaiBundle>,
    },
    PackageSignerTrusted,
    PackageActivated {
        bundle: Box<crate::FaiBundle>,
    },
    Packages {
        packages: Vec<crate::FaiPackageStatus>,
    },
    AgentRun {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<AgentRunStatus>,
    },
    AgentRuns {
        runs: Vec<AgentRunStatus>,
    },
    AgentRunEvents {
        run_id: String,
        events: Vec<AgentRunEvent>,
        state: AgentRunState,
    },
    Agents {
        agents: Vec<AgentDefinition>,
    },
    AgentControlStatuses {
        agents: Vec<AgentControlStatus>,
    },
    AgentDryRun {
        report: AgentDryRunReport,
    },
    Workflows {
        workflows: Vec<crate::WorkflowDefinition>,
    },
    WorkflowStarted {
        run_id: String,
    },
    WorkflowRuns {
        runs: Vec<crate::WorkflowRunStatus>,
    },
    WorkflowRun {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<crate::WorkflowRunStatus>,
    },
    WorkflowControl {
        run_id: String,
        accepted: bool,
    },
    CapabilityPreview {
        preview: crate::CapabilityPreview,
    },
    CapabilityLeases {
        leases: Vec<crate::CapabilityLease>,
    },
    CapabilityRevocation {
        lease_id: String,
        revoked: bool,
    },
    ContextPublished {
        envelope: crate::ContextEnvelope,
    },
    ContextGranted {
        grant: crate::ContextGrant,
    },
    ContextState {
        envelopes: Vec<crate::ContextEnvelope>,
        grants: Vec<crate::ContextGrant>,
        suggestions: Vec<crate::ContextSuggestion>,
    },
    ContextGrantRevocation {
        grant_id: String,
        revoked: bool,
    },
    ContextCleared {
        cleared: usize,
    },
    SuggestionPublished {
        suggestion: crate::ContextSuggestion,
    },
    SuggestionDismissed {
        suggestion_id: String,
        dismissed: bool,
    },
    IntentRouted {
        route: crate::IntentRoute,
    },
    RoutineState {
        state: crate::RoutineStateSnapshot,
    },
    RoutineEvaluated {
        evaluations: Vec<crate::RoutineEvaluation>,
    },
    RoutinesSuspended {
        suspended: bool,
    },
    RoutineSuggestionDismissed {
        suggestion_id: String,
        dismissed: bool,
    },
    RoutineSuggestionPromoted {
        outcome: crate::RoutinePromotionOutcome,
    },
    EventFabricState {
        state: crate::EventFabricSnapshot,
    },
    EventSourceConfigured {
        policy: crate::EventSourcePolicy,
    },
    EventFabricConnection {
        connected: bool,
    },
    EventDelivered {
        delivery: crate::EventDelivery,
    },
    EventJournalCleared {
        cleared: usize,
    },
    Connectors {
        connectors: Vec<crate::ConnectorStatus>,
    },
    ConnectorChanged {
        status: crate::ConnectorStatus,
    },
    AgentRunCancellation {
        run_id: String,
        accepted: bool,
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
            Ok(AiIpcRequest::IngestDocument { path }) => {
                let source = path.display().to_string();
                match service.ingest_document(path).await {
                    Ok(result) => {
                        tracing::info!(
                            target: "focaldesk.ai",
                            source = %result.source,
                            chunks = result.chunks,
                            "document indexed"
                        );
                        AiIpcResponse::DocumentIngested { result }
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "focaldesk.ai",
                            %source,
                            error = %err,
                            "document indexing failed"
                        );
                        AiIpcResponse::Error {
                            message: err.to_string(),
                        }
                    }
                }
            }
            Ok(AiIpcRequest::IngestDirectory { path, recursive }) => {
                let source = path.display().to_string();
                match service.ingest_directory(path, recursive).await {
                    Ok(result) => {
                        tracing::info!(
                            target: "focaldesk.ai",
                            source = %result.source,
                            indexed = result.indexed,
                            unchanged = result.unchanged,
                            failed = result.failed,
                            "directory indexed"
                        );
                        AiIpcResponse::DirectoryIngested { result }
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "focaldesk.ai",
                            %source,
                            error = %err,
                            "directory indexing failed"
                        );
                        AiIpcResponse::Error {
                            message: err.to_string(),
                        }
                    }
                }
            }
            Ok(AiIpcRequest::ListIndexedDocuments) => match service.indexed_documents().await {
                Ok(documents) => AiIpcResponse::IndexedDocuments {
                    documents: indexed_document_summaries(documents),
                },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RemoveIndexedDocument { source }) => {
                match service.remove_document(source.clone()).await {
                    Ok(removed) => AiIpcResponse::IndexedDocumentRemoved { source, removed },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::EvaluateRetrieval { cases, top_k }) => {
                match service.evaluate_retrieval(cases, top_k).await {
                    Ok(report) => AiIpcResponse::RetrievalEvaluated { report },
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
            Ok(AiIpcRequest::StartAgent { request }) => match service.start_agent(request).await {
                Ok(run_id) => AiIpcResponse::AgentStarted { run_id },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RetryAgentRun { run_id }) => {
                match service.retry_agent_run(&run_id).await {
                    Ok(run_id) => AiIpcResponse::AgentStarted { run_id },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::FireAgentTrigger {
                agent_id,
                trigger_id,
            }) => match service.fire_agent_trigger(&agent_id, &trigger_id) {
                Ok(run_id) => AiIpcResponse::AgentStarted { run_id },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::DispatchAgentEvent { kind, value }) => {
                match service.dispatch_agent_event(kind, &value) {
                    Ok(run_ids) => AiIpcResponse::AgentTriggersStarted { run_ids },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::GetAgentTriggerState) => AiIpcResponse::AgentTriggerState {
                suspended: service.triggers_suspended(),
            },
            Ok(AiIpcRequest::GetMissionControl { query, limit }) => {
                match service.mission_control_snapshot(query.as_deref(), limit) {
                    Ok(state) => AiIpcResponse::MissionControlState { state },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ActivateMissionControlPause) => {
                match service.activate_mission_control_pause() {
                    Ok(state) => AiIpcResponse::MissionControlState { state },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::EvaluateScenario { fixture }) => {
                match crate::evaluate_scenario(*fixture) {
                    Ok(report) => AiIpcResponse::ScenarioEvaluated { report },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::CaptureScenario { name, query, limit }) => service
                .mission_control_snapshot(query.as_deref(), limit)
                .and_then(|snapshot| {
                    crate::ScenarioFixture::from_timeline(name, &snapshot.timeline)
                })
                .map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |fixture| AiIpcResponse::ScenarioCaptured { fixture },
                ),
            Ok(AiIpcRequest::InspectPackage { bundle }) => {
                service.inspect_package(&bundle).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |inspection| AiIpcResponse::PackageInspected { inspection },
                )
            }
            Ok(AiIpcRequest::GeneratePackageSigner { signer_id }) => {
                service.generate_package_signer(&signer_id).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |signer| AiIpcResponse::PackageSignerGenerated { signer },
                )
            }
            Ok(AiIpcRequest::BuildPackageProject { project }) => {
                service.build_package_project(&project).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |bundle| AiIpcResponse::PackageBuilt {
                        bundle: Box::new(bundle),
                    },
                )
            }
            Ok(AiIpcRequest::TrustPackageSigner { signer }) => {
                service.trust_package_signer(signer).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |()| AiIpcResponse::PackageSignerTrusted,
                )
            }
            Ok(AiIpcRequest::StagePackage { bundle }) => {
                service.stage_package(*bundle).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |inspection| AiIpcResponse::PackageInspected { inspection },
                )
            }
            Ok(AiIpcRequest::ActivatePackage { package_id }) => {
                service.activate_package(&package_id).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |bundle| AiIpcResponse::PackageActivated {
                        bundle: Box::new(bundle),
                    },
                )
            }
            Ok(AiIpcRequest::RollbackPackage { package_id }) => {
                service.rollback_package(&package_id).map_or_else(
                    |err| AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                    |bundle| AiIpcResponse::PackageActivated {
                        bundle: Box::new(bundle),
                    },
                )
            }
            Ok(AiIpcRequest::ListPackages) => service.package_statuses().map_or_else(
                |err| AiIpcResponse::Error {
                    message: err.to_string(),
                },
                |packages| AiIpcResponse::Packages { packages },
            ),
            Ok(AiIpcRequest::SetAgentTriggersSuspended { suspended }) => {
                match service.set_triggers_suspended(suspended) {
                    Ok(()) => AiIpcResponse::AgentTriggerState { suspended },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::GetAgentRun { run_id }) => match service.agent_run_status(&run_id) {
                Ok(status) => AiIpcResponse::AgentRun { run_id, status },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::WatchAgentRun {
                run_id,
                after_sequence,
            }) => match service.watch_agent_run(&run_id, after_sequence).await {
                Ok((events, state)) => AiIpcResponse::AgentRunEvents {
                    run_id,
                    events,
                    state,
                },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ListAgentRuns) => match service.agent_runs() {
                Ok(runs) => AiIpcResponse::AgentRuns { runs },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ListAgents) => AiIpcResponse::Agents {
                agents: service.agent_definitions(),
            },
            Ok(AiIpcRequest::ReloadAgents) => match service.reload_agent_definitions() {
                Ok(agents) => AiIpcResponse::Agents { agents },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::InstallAgent {
                definition,
                overwrite,
            }) => match service.install_agent_package(&definition, overwrite) {
                Ok(agents) => AiIpcResponse::Agents { agents },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::GetAgentControlStatuses) => match service.agent_control_statuses() {
                Ok(agents) => AiIpcResponse::AgentControlStatuses { agents },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::SetAgentEnabled { agent_id, enabled }) => {
                match service.set_agent_enabled(&agent_id, enabled) {
                    Ok(()) => match service.agent_control_statuses() {
                        Ok(agents) => AiIpcResponse::AgentControlStatuses { agents },
                        Err(err) => AiIpcResponse::Error {
                            message: err.to_string(),
                        },
                    },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::RollbackAgent { agent_id }) => {
                match service.rollback_agent_package(&agent_id) {
                    Ok(agents) => AiIpcResponse::Agents { agents },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::DryRunAgent { request }) => {
                match service.dry_run_agent(request).await {
                    Ok(report) => AiIpcResponse::AgentDryRun { report },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ListWorkflows) => AiIpcResponse::Workflows {
                workflows: service.workflow_definitions(),
            },
            Ok(AiIpcRequest::StartWorkflow { workflow_id }) => {
                match service.start_workflow(&workflow_id) {
                    Ok(run_id) => AiIpcResponse::WorkflowStarted { run_id },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ListWorkflowRuns) => match service.workflow_runs() {
                Ok(runs) => AiIpcResponse::WorkflowRuns { runs },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::GetWorkflowRun { run_id }) => match service.workflow_run(&run_id) {
                Ok(status) => AiIpcResponse::WorkflowRun { run_id, status },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::SetWorkflowPaused { run_id, paused }) => {
                match service.set_workflow_paused(&run_id, paused) {
                    Ok(()) => AiIpcResponse::WorkflowControl {
                        run_id,
                        accepted: true,
                    },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::CancelWorkflow { run_id }) => match service.cancel_workflow(&run_id) {
                Ok(accepted) => AiIpcResponse::WorkflowControl { run_id, accepted },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RetryWorkflow { run_id }) => match service.retry_workflow(&run_id) {
                Ok(run_id) => AiIpcResponse::WorkflowStarted { run_id },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::PreviewCapabilities { agent_id, ceiling }) => {
                match service.capability_preview(&agent_id, ceiling.as_ref()) {
                    Ok(preview) => AiIpcResponse::CapabilityPreview { preview },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ListCapabilityLeases) => match service.capability_leases() {
                Ok(leases) => AiIpcResponse::CapabilityLeases { leases },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RevokeCapabilityLease { lease_id }) => {
                match service.revoke_capability_lease(&lease_id) {
                    Ok(revoked) => AiIpcResponse::CapabilityRevocation { lease_id, revoked },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::PublishContext {
                kind,
                provenance,
                sensitivity,
                payload,
                ttl_seconds,
            }) => {
                match service.publish_context(kind, provenance, sensitivity, payload, ttl_seconds) {
                    Ok(envelope) => AiIpcResponse::ContextPublished { envelope },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::GrantContext {
                agent_id,
                kinds,
                ttl_seconds,
            }) => match service.grant_context(agent_id, kinds, ttl_seconds) {
                Ok(grant) => AiIpcResponse::ContextGranted { grant },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::GetContextState) => match service.context_snapshot() {
                Ok((envelopes, grants, suggestions)) => AiIpcResponse::ContextState {
                    envelopes,
                    grants,
                    suggestions,
                },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RevokeContextGrant { grant_id }) => {
                match service.revoke_context_grant(&grant_id) {
                    Ok(revoked) => AiIpcResponse::ContextGrantRevocation { grant_id, revoked },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ClearContext) => match service.clear_context() {
                Ok(cleared) => AiIpcResponse::ContextCleared { cleared },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::PublishSuggestion {
                agent_id,
                title,
                body,
                ttl_seconds,
            }) => match service.publish_suggestion(agent_id, title, body, ttl_seconds) {
                Ok(suggestion) => AiIpcResponse::SuggestionPublished { suggestion },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::DismissSuggestion { suggestion_id }) => {
                match service.dismiss_suggestion(&suggestion_id) {
                    Ok(dismissed) => AiIpcResponse::SuggestionDismissed {
                        suggestion_id,
                        dismissed,
                    },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::RouteIntent { text }) => AiIpcResponse::IntentRouted {
                route: crate::route_intent(&text),
            },
            Ok(AiIpcRequest::GetRoutineState) => match service.routine_state() {
                Ok(state) => AiIpcResponse::RoutineState { state },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::DispatchRoutineEvent { event }) => {
                match service.dispatch_routine_event(event) {
                    Ok(evaluations) => AiIpcResponse::RoutineEvaluated { evaluations },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::SimulateRoutineEvent { event }) => {
                match service.simulate_routine_event(event) {
                    Ok(evaluations) => AiIpcResponse::RoutineEvaluated { evaluations },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::SetRoutinesSuspended { suspended }) => {
                match service.set_routines_suspended(suspended) {
                    Ok(suspended) => AiIpcResponse::RoutinesSuspended { suspended },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::DismissRoutineSuggestion { suggestion_id }) => {
                match service.dismiss_routine_suggestion(&suggestion_id) {
                    Ok(dismissed) => AiIpcResponse::RoutineSuggestionDismissed {
                        suggestion_id,
                        dismissed,
                    },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::PromoteRoutineSuggestion { suggestion_id }) => {
                match service.promote_routine_suggestion(&suggestion_id).await {
                    Ok(outcome) => AiIpcResponse::RoutineSuggestionPromoted { outcome },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::GetEventFabricState) => match service.event_fabric_state() {
                Ok(state) => AiIpcResponse::EventFabricState { state },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ConfigureEventSource { policy }) => {
                match service.configure_event_source(policy) {
                    Ok(policy) => AiIpcResponse::EventSourceConfigured { policy },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::SetEventFabricConnected { connected }) => {
                match service.set_event_fabric_connected(connected) {
                    Ok(connected) => AiIpcResponse::EventFabricConnection { connected },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ListConnectors) => match service.connector_statuses() {
                Ok(connectors) => AiIpcResponse::Connectors { connectors },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::InstallConnector {
                manifest,
                overwrite,
            }) => match service.install_connector(manifest, overwrite) {
                Ok(status) => AiIpcResponse::ConnectorChanged { status },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::SetConnectorEnabled {
                connector_id,
                enabled,
                network_allowed,
            }) => match service.set_connector_enabled(&connector_id, enabled, network_allowed) {
                Ok(status) => AiIpcResponse::ConnectorChanged { status },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::RollbackConnector { connector_id }) => {
                match service.rollback_connector(&connector_id) {
                    Ok(status) => AiIpcResponse::ConnectorChanged { status },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::PublishConnectorEvent { request }) => {
                match service.ingest_connector_event(request) {
                    Ok(delivery) => AiIpcResponse::EventDelivered { delivery },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::PublishManagedConnectorEvent {
                connector_id,
                source,
                payload,
            }) => match service.ingest_managed_connector_event(connector_id, source, payload) {
                Ok(delivery) => AiIpcResponse::EventDelivered { delivery },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::SimulateEvent {
                source,
                producer,
                payload,
            }) => match service.simulate_event(source, producer, payload) {
                Ok(delivery) => AiIpcResponse::EventDelivered { delivery },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::ReplayEventSimulation { event_id }) => {
                match service.replay_event_simulation(&event_id) {
                    Ok(delivery) => AiIpcResponse::EventDelivered { delivery },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(AiIpcRequest::ClearEventJournal) => match service.clear_event_journal() {
                Ok(cleared) => AiIpcResponse::EventJournalCleared { cleared },
                Err(err) => AiIpcResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(AiIpcRequest::CancelAgentRun { run_id }) => {
                match service.cancel_agent_run(&run_id) {
                    Ok(accepted) => AiIpcResponse::AgentRunCancellation { run_id, accepted },
                    Err(err) => AiIpcResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
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
                        message: format!("{err:#}"),
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
    configure_ai_stream(&stream, AI_RESPONSE_TIMEOUT)?;
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
        let (response, mode) = decode_ai_response(&frame, Some(request_id))?;
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

fn configure_ai_stream(stream: &StdUnixStream, response_timeout: Duration) -> Result<()> {
    transport::configure_stream(stream).context("configure AI IPC connection")?;
    stream
        .set_read_timeout(Some(response_timeout))
        .context("configure AI IPC response timeout")
}

fn response_timeout_for(request: &AiIpcRequest) -> Duration {
    match request {
        AiIpcRequest::IngestDocument { .. } | AiIpcRequest::IngestDirectory { .. } => {
            AI_INGEST_RESPONSE_TIMEOUT
        }
        _ => AI_RESPONSE_TIMEOUT,
    }
}

fn indexed_document_summaries(
    mut documents: Vec<focaldesk_memory::IndexedDocument>,
) -> Vec<focaldesk_memory::IndexedDocument> {
    // Chunk ids are an internal deletion detail and can dominate the source
    // list payload for heavily chunked repositories. Callers only need the
    // document metadata and chunk count.
    for document in &mut documents {
        document.memory_ids.clear();
    }
    documents
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
    let response_timeout = response_timeout_for(request);
    configure_ai_stream(&stream, response_timeout)?;
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

    read_ai_response(&mut stream, response_timeout, request_id)
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
        if let Err(err) = configure_ai_stream(&client, AI_RESPONSE_TIMEOUT) {
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
    fn ingestion_uses_a_long_running_response_timeout() {
        assert_eq!(
            response_timeout_for(&AiIpcRequest::IngestDirectory {
                path: PathBuf::from("project"),
                recursive: true,
            }),
            AI_INGEST_RESPONSE_TIMEOUT
        );
        assert_eq!(
            response_timeout_for(&AiIpcRequest::IngestDocument {
                path: PathBuf::from("large.pdf"),
            }),
            AI_INGEST_RESPONSE_TIMEOUT
        );
        assert!(AI_INGEST_RESPONSE_TIMEOUT > AI_RESPONSE_TIMEOUT);
    }

    #[test]
    fn indexed_document_list_omits_internal_chunk_ids() {
        let documents = indexed_document_summaries(vec![focaldesk_memory::IndexedDocument {
            source: "/project/src/main.rs".into(),
            title: "main.rs".into(),
            media_type: "text/rust".into(),
            content_hash: "abc".into(),
            modified_at_unix: 1,
            indexed_at_unix: 2,
            chunk_count: 3,
            memory_ids: vec![10, 11, 12],
        }]);
        assert_eq!(documents[0].chunk_count, 3);
        assert!(documents[0].memory_ids.is_empty());
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
            citations: Vec::new(),
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
                citations: Vec::new(),
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
    fn asynchronous_agent_start_round_trips_without_losing_profile() {
        let encoded = encode_ai_request(
            &AiIpcRequest::StartAgent {
                request: AgentRequest {
                    objective: "Inspect the current workspace".into(),
                    agent_id: Some("accessibility".into()),
                    provider: Some("ollama".into()),
                    model: None,
                },
            },
            "agent-start-1",
        )
        .unwrap();
        let (_, request) = decode_ai_request(&encoded);
        let AiIpcRequest::StartAgent { request } = request.unwrap() else {
            panic!("expected asynchronous agent request");
        };
        assert_eq!(request.agent_id.as_deref(), Some("accessibility"));
        assert_eq!(request.objective, "Inspect the current workspace");
    }

    #[test]
    fn mission_control_request_round_trips_bounded_search() {
        let encoded = encode_ai_request(
            &AiIpcRequest::GetMissionControl {
                query: Some("workflow failed".into()),
                limit: 75,
            },
            "mission-control-1",
        )
        .unwrap();
        let (_, request) = decode_ai_request(&encoded);
        let AiIpcRequest::GetMissionControl { query, limit } = request.unwrap() else {
            panic!("expected Mission Control request");
        };
        assert_eq!(query.as_deref(), Some("workflow failed"));
        assert_eq!(limit, 75);
    }

    #[test]
    fn scenario_capture_request_round_trips_without_live_inputs() {
        let encoded = encode_ai_request(
            &AiIpcRequest::CaptureScenario {
                name: "ci-trace".into(),
                query: Some("failed".into()),
                limit: 25,
            },
            "scenario-capture-1",
        )
        .unwrap();
        let (_, request) = decode_ai_request(&encoded);
        let AiIpcRequest::CaptureScenario { name, query, limit } = request.unwrap() else {
            panic!("expected Scenario Lab capture request");
        };
        assert_eq!(name, "ci-trace");
        assert_eq!(query.as_deref(), Some("failed"));
        assert_eq!(limit, 25);
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
