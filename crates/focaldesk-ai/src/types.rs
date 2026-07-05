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
}

impl ChatRequest {
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        Self {
            provider: None,
            model: None,
            messages: vec![ChatMessage::user(prompt)],
            temperature: None,
            max_tokens: None,
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
pub struct AiDaemonStatus {
    pub active_requests: u32,
    pub default_provider: String,
    pub provider_count: usize,
}
