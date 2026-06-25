use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::AiProvider;
use crate::types::{ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo, ProviderModelInfo};

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    api_key: String,
    default_model: Option<String>,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String, default_model: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .context("failed to build Anthropic HTTP client")?;

        Ok(Self {
            api_key,
            default_model,
            client,
        })
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "anthropic".into(),
            kind: "anthropic".into(),
            base_url: Some("https://api.anthropic.com".into()),
            default_model: self.default_model.clone(),
        }
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        Ok(self
            .default_model
            .iter()
            .cloned()
            .map(|id| ProviderModelInfo { id })
            .collect())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let model = request
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .ok_or_else(|| anyhow!("no model configured for Anthropic provider"))?;

        if request.messages.is_empty() {
            bail!("chat request must include at least one message");
        }

        let system = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, ChatRole::System))
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let messages = request
            .messages
            .iter()
            .filter(|message| !matches!(message.role, ChatRole::System))
            .map(AnthropicMessage::from)
            .collect::<Vec<_>>();

        let payload = AnthropicChatRequest {
            model: model.clone(),
            messages,
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            temperature: request.temperature,
            max_tokens: request.max_tokens.unwrap_or(1024),
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&payload)
            .send()
            .await
            .context("Anthropic chat request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Anthropic response body")?;

        if !status.is_success() {
            bail!("Anthropic returned HTTP {}: {}", status, body);
        }

        let decoded: AnthropicChatResponse =
            serde_json::from_str(&body).context("failed to parse Anthropic chat response")?;
        let content = decoded
            .content
            .into_iter()
            .filter_map(|block| match block {
                AnthropicContentBlock::Text { text } => Some(text),
                AnthropicContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("");

        if content.is_empty() {
            bail!("Anthropic response did not include text content");
        }

        Ok(ChatResponse {
            provider: "anthropic".into(),
            model: Some(model),
            content,
        })
    }
}

#[derive(Debug, Serialize)]
struct AnthropicChatRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    max_tokens: u32,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: String,
}

impl From<&ChatMessage> for AnthropicMessage {
    fn from(message: &ChatMessage) -> Self {
        let role = match message.role {
            ChatRole::Assistant => "assistant",
            ChatRole::System | ChatRole::User => "user",
        };
        Self {
            role,
            content: message.content.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicChatResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}
