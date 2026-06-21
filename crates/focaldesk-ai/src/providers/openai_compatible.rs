use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::AiProvider;
use crate::types::{ChatMessage, ChatRequest, ChatResponse, ProviderInfo};

#[derive(Debug, Clone)]
pub struct OpenAICompatibleProvider {
    id: String,
    kind: String,
    base_url: String,
    api_key: Option<String>,
    default_model: Option<String>,
    client: Client,
}

pub type OpenAIProvider = OpenAICompatibleProvider;
pub type VllmProvider = OpenAICompatibleProvider;

impl OpenAICompatibleProvider {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: Option<String>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .context("failed to build OpenAI-compatible HTTP client")?;

        Ok(Self {
            id: id.into(),
            kind: kind.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            default_model,
            client,
        })
    }

    pub fn openai(api_key: String, default_model: Option<String>) -> Result<OpenAIProvider> {
        Self::new(
            "openai",
            "openai",
            "https://api.openai.com/v1",
            Some(api_key),
            default_model,
        )
    }

    pub fn vllm(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: Option<String>,
    ) -> Result<VllmProvider> {
        Self::new("vllm", "vllm", base_url, api_key, default_model)
    }
}

#[async_trait]
impl AiProvider for OpenAICompatibleProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            kind: self.kind.clone(),
            base_url: Some(self.base_url.clone()),
            default_model: self.default_model.clone(),
        }
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let model = request
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .ok_or_else(|| anyhow!("no model configured for provider {}", self.id))?;

        if request.messages.is_empty() {
            bail!("chat request must include at least one message");
        }

        let payload = OpenAIChatRequest {
            model: model.clone(),
            messages: request
                .messages
                .iter()
                .map(OpenAIChatMessage::from)
                .collect(),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        };

        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&payload);

        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = builder
            .send()
            .await
            .with_context(|| format!("{} chat request failed", self.id))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read OpenAI-compatible response body")?;

        if !status.is_success() {
            bail!("{} returned HTTP {}: {}", self.id, status, body);
        }

        let decoded: OpenAIChatResponse = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {} chat response", self.id))?;
        let content = decoded
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| anyhow!("{} response did not include message content", self.id))?;

        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content,
        })
    }
}

#[derive(Debug, Serialize)]
struct OpenAIChatRequest {
    model: String,
    messages: Vec<OpenAIChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OpenAIChatMessage {
    role: &'static str,
    content: String,
}

impl From<&ChatMessage> for OpenAIChatMessage {
    fn from(message: &ChatMessage) -> Self {
        Self {
            role: message.role.as_str(),
            content: message.content.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    content: String,
}
