use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::AiProvider;
use crate::types::{ChatMessage, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo};

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    id: String,
    base_url: String,
    default_model: Option<String>,
    client: Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, default_model: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build Ollama HTTP client")?;

        Ok(Self {
            id: "ollama".into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            default_model,
            client,
        })
    }
}

#[async_trait]
impl AiProvider for OllamaProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            kind: "ollama".into(),
            base_url: Some(self.base_url.clone()),
            default_model: self.default_model.clone(),
        }
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .context("Ollama model list request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Ollama model list response body")?;

        if !status.is_success() {
            bail!(
                "Ollama returned HTTP {} while listing models: {}",
                status,
                body
            );
        }

        let decoded: OllamaTagsResponse =
            serde_json::from_str(&body).context("failed to parse Ollama model list")?;

        Ok(decoded
            .models
            .into_iter()
            .map(|model| ProviderModelInfo { id: model.name })
            .filter(|model| !model.id.trim().is_empty())
            .collect())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let model = self.resolve_model(request.model.clone()).await?;

        if request.messages.is_empty() {
            bail!("chat request must include at least one message");
        }

        let payload = OllamaChatRequest {
            model: model.clone(),
            messages: request.messages.iter().map(OllamaMessage::from).collect(),
            stream: false,
            options: OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            },
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&payload)
            .send()
            .await
            .context("Ollama chat request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Ollama response body")?;

        if !status.is_success() {
            bail!("Ollama returned HTTP {}: {}", status, body);
        }

        let decoded: OllamaChatResponse =
            serde_json::from_str(&body).context("failed to parse Ollama chat response")?;

        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content: decoded.message.content,
        })
    }
}

impl OllamaProvider {
    async fn resolve_model(&self, request_model: Option<String>) -> Result<String> {
        if let Some(model) = request_model.or_else(|| self.default_model.clone()) {
            return Ok(model);
        }

        self.list_models()
            .await?
            .into_iter()
            .map(|model| model.id)
            .find(|name| !name.trim().is_empty())
            .ok_or_else(|| anyhow!("no Ollama model configured and no installed models found"))
    }
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: &'static str,
    content: String,
}

impl From<&ChatMessage> for OllamaMessage {
    fn from(message: &ChatMessage) -> Self {
        Self {
            role: message.role.as_str(),
            content: message.content.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    name: String,
}
