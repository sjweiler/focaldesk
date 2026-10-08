pub mod agent;
pub mod agent_definition;
pub mod agent_sdk;
pub mod capability;
pub mod connector;
pub mod context;
pub mod event;
pub mod forge;
pub mod ipc;
mod managed_provider;
pub mod mission;
pub mod package;
mod permissions;
pub mod planner;
pub mod private_registry;
pub mod provider;
pub mod providers;
pub mod routine;
mod run_store;
pub mod scenario;
pub mod service;
pub mod types;
pub mod workflow;

#[cfg(test)]
mod test_support;

pub use agent::{
    Agent, AgentActionResponse, AgentConfirmation, AgentDryRunReport, AgentEventSink,
    AgentProposedAction, AgentRequest, AgentResponse, AgentRunEvent, AgentRunEventKind,
    AgentRunState, AgentRunStatus, AgentStepResult, AgentToolExecutor, AgentToolSpec,
    AgentTriggerSource,
};
pub use agent_definition::{
    AGENT_MANIFEST_VERSION, AgentDefinition, AgentTrigger, AgentTriggerKind, built_in_agents,
    install_agent_definition, load_agent_definitions, rollback_agent_definition,
};
pub use agent_sdk::{AgentBuilder, MockAgentTools, ScriptedAgentProvider};
pub use capability::{CapabilityLease, CapabilityPolicy, CapabilityPreview};
pub use connector::{
    CONNECTOR_MANIFEST_VERSION, ConnectorEventRequest, ConnectorHealth, ConnectorManifest,
    ConnectorRegistry, ConnectorRuntimeManifest, ConnectorStatus, built_in_connectors,
    random_connector_nonce, sign_connector_event,
};
pub use context::{
    ContextBroker, ContextEnvelope, ContextGrant, ContextKind, ContextSensitivity,
    ContextSuggestion, IntentDestination, IntentRoute, route_intent,
};
pub use event::{
    EventDelivery, EventEnvelope, EventFabric, EventFabricSnapshot, EventSource,
    EventSourceDescriptor, EventSourcePolicy, supported_event_fields,
};
pub use focaldesk_memory::{IndexedDocument, MemoryId, MemoryStatus, SearchHit};
pub use forge::{
    FaiForgeManifest, FaiForgeProject, FaiForgeReport, FaiLocalRegistry, FaiRegistryEntry,
    build_fai_project,
};
pub use ipc::{
    AI_LEGACY_PROTOCOL_VERSION, AI_MAX_REQUEST_BYTES, AI_MAX_RESPONSE_BYTES, AI_PROTOCOL_VERSION,
    AI_SOCKET_ENV, AI_SOCKET_NAME, AiIpcRequest, AiIpcResponse, ai_socket_path, cancel_ai_stream,
    send_ai_request, serve_ai_ipc, stream_ai_chat,
};
pub use mission::{
    MissionBudgetSummary, MissionConnectorSummary, MissionControlSnapshot, MissionRuntimeSummary,
    MissionTimelineEntry, MissionTimelineKind,
};
pub use package::{
    FAI_PACKAGE_VERSION, FaiAuthoritySummary, FaiBundle, FaiPackageDependency,
    FaiPackageInspection, FaiPackageManager, FaiPackageManifest, FaiPackagePayload,
    FaiPackageStatus, FaiSigner, fai_signer_public_key_hex, fai_signer_secret_handle,
    sign_fai_bundle,
};
pub use permissions::{AiPermissionRecord, list_ai_permission_records, revoke_ai_permission};
pub use planner::Planner;
pub use private_registry::{
    FaiCatalogEntry, FaiCatalogRevocation, FaiPackageLock, FaiPackageLockEntry, FaiPrivateRegistry,
    FaiRegistryCatalog, FaiRegistryCompatibility, FaiRegistryPolicy, FaiSignedCatalog,
    PRIVATE_REGISTRY_VERSION, approve_registry_signer, compare_catalog_versions,
    download_registry_package, fetch_registry_catalog, publish_registry_package,
    resolve_catalog_lock, revoke_registry_package, verify_registry_catalog,
};
pub use provider::{AiProvider, ProviderError, ProviderErrorKind};
pub use routine::{
    AttentionPriority, QuietHours, RoutineDefinition, RoutineEngine, RoutineEvaluation,
    RoutineEvent, RoutineEventKind, RoutinePromotion, RoutinePromotionOutcome,
    RoutineStateSnapshot, RoutineSuggestion, built_in_routines,
};
pub use scenario::{
    SCENARIO_VERSION, ScenarioCheck, ScenarioConnectorPolicy, ScenarioFixture,
    ScenarioInitialState, ScenarioLeaseState, ScenarioReport, ScenarioRouteObservation,
    ScenarioStep, evaluate_scenario,
};
pub use service::{AgentControlStatus, AiService};
pub use types::{
    AiDaemonStatus, AiStreamEvent, ChatMessage, ChatRequest, ChatResponse, ChatRole, Citation,
    DirectoryIngestResult, DocumentIngestResult, ProviderInfo, ProviderModelInfo,
    ProviderTelemetry, RetrievalEvalCase, RetrievalEvalReport, TokenUsage,
};
pub use workflow::{
    WORKFLOW_MANIFEST_VERSION, WorkflowArtifact, WorkflowDefinition, WorkflowNode,
    WorkflowNodeState, WorkflowNodeStatus, WorkflowRunState, WorkflowRunStatus, built_in_workflows,
};
