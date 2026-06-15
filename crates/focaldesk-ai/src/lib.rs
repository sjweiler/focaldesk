pub mod agent;
pub mod ipc;
pub mod planner;
pub mod provider;
pub mod providers;
pub mod service;
pub mod types;

pub use agent::Agent;
pub use ipc::{AI_SOCKET_PATH, AiIpcRequest, AiIpcResponse, send_ai_request, serve_ai_ipc};
pub use planner::Planner;
pub use provider::AiProvider;
pub use service::AiService;
pub use types::{ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo};
