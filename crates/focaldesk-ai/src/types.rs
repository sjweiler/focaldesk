use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// When true, the latest user message is used to recall similar entries
    /// from the AI memory store and prepend them as context before the
    /// provider is called. No-op if no memory store is configured.
    #[serde(default)]
    pub use_memory: bool,
}

impl ChatRequest {
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        Self {
            provider: None,
            model: None,
            messages: vec![ChatMessage::user(prompt)],
            temperature: None,
            max_tokens: None,
            use_memory: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl ChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub provider: String,
    pub model: Option<String>,
    pub content: String,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderTelemetry {
    pub provider: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub cancellations: u64,
    pub timeouts: u64,
    pub retries: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_latency_ms: u64,
    pub last_latency_ms: Option<u64>,
    pub last_success_at_unix: Option<u64>,
    pub last_failure_at_unix: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiDaemonStatus {
    pub active_requests: u32,
    /// Requests currently blocked on an interactive AI permission decision.
    #[serde(default)]
    pub pending_permissions: u32,
    pub default_provider: String,
    pub provider_count: usize,
    #[serde(default)]
    pub provider_telemetry: Vec<ProviderTelemetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AiStreamEvent {
    Started {
        request_id: String,
        provider: String,
        model: Option<String>,
    },
    Delta {
        request_id: String,
        content: String,
    },
    Completed {
        request_id: String,
        response: ChatResponse,
    },
    Failed {
        request_id: String,
        message: String,
    },
    Cancelled {
        request_id: String,
    },
}
