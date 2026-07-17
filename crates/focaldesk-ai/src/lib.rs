pub mod agent;
pub mod ipc;
mod permissions;
pub mod planner;
pub mod provider;
pub mod providers;
pub mod service;
pub mod types;

pub use agent::Agent;
pub use focaldesk_memory::{MemoryId, SearchHit};
pub use ipc::{
    AI_SOCKET_ENV, AI_SOCKET_NAME, AiIpcRequest, AiIpcResponse, ai_socket_path, send_ai_request,
    serve_ai_ipc,
};
pub use permissions::{AiPermissionRecord, list_ai_permission_records, revoke_ai_permission};
pub use planner::Planner;
pub use provider::AiProvider;
pub use service::AiService;
pub use types::{
    AiDaemonStatus, ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo,
    ProviderModelInfo,
};
