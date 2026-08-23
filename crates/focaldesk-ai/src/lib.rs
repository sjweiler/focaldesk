pub mod agent;
pub mod ipc;
mod managed_provider;
mod permissions;
pub mod planner;
pub mod provider;
pub mod providers;
pub mod service;
pub mod types;

#[cfg(test)]
mod test_support;

pub use agent::{
    Agent, AgentActionResponse, AgentConfirmation, AgentProposedAction, AgentRequest,
    AgentResponse, AgentStepResult, AgentToolExecutor, AgentToolSpec,
};
pub use focaldesk_memory::{MemoryId, MemoryStatus, SearchHit};
pub use ipc::{
    AI_LEGACY_PROTOCOL_VERSION, AI_MAX_REQUEST_BYTES, AI_MAX_RESPONSE_BYTES, AI_PROTOCOL_VERSION,
    AI_SOCKET_ENV, AI_SOCKET_NAME, AiIpcRequest, AiIpcResponse, ai_socket_path, cancel_ai_stream,
    send_ai_request, serve_ai_ipc, stream_ai_chat,
};
pub use permissions::{AiPermissionRecord, list_ai_permission_records, revoke_ai_permission};
pub use planner::Planner;
pub use provider::{AiProvider, ProviderError, ProviderErrorKind};
pub use service::AiService;
pub use types::{
    AiDaemonStatus, AiStreamEvent, ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo,
    ProviderModelInfo, ProviderTelemetry, TokenUsage,
};
