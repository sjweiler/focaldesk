use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::{AiProvider, ProviderError, retry_after};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo, TokenUsage,
};

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

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        let mut builder = self.client.get(format!("{}/models", self.base_url));

        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = builder.send().await.map_err(|error| {
            ProviderError::from_transport(&format!("{} model list request failed", self.id), error)
        })?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.map_err(|error| {
            ProviderError::from_transport("failed to read model list response body", error)
        })?;

        if !status.is_success() {
            return Err(ProviderError::from_http(&self.id, status, &body)
                .with_retry_after(retry_after)
                .into());
        }

        let decoded: OpenAIModelsResponse = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {} model list", self.id))?;

        Ok(decoded
            .data
            .into_iter()
            .map(|model| ProviderModelInfo { id: model.id })
            .filter(|model| !model.id.trim().is_empty())
            .collect())
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

        let response = builder.send().await.map_err(|error| {
            ProviderError::from_transport(&format!("{} chat request failed", self.id), error)
        })?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.map_err(|error| {
            ProviderError::from_transport("failed to read OpenAI-compatible response body", error)
        })?;

        if !status.is_success() {
            return Err(ProviderError::from_http(&self.id, status, &body)
                .with_retry_after(retry_after)
                .into());
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
            usage: decoded.usage.map(|usage| TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            }),
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
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelInfo {
    id: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderErrorKind, provider_error};
    use crate::test_support::serve_once;

    #[tokio::test]
    async fn openai_chat_contract_preserves_messages_options_auth_and_usage() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"choices":[{"message":{"content":"contract-ok"}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        )
        .await;
        let provider = OpenAICompatibleProvider::new(
            "openai-test",
            "openai",
            format!("{base_url}/v1/"),
            Some("secret-test-key".into()),
            Some("default-model".into()),
        )
        .unwrap();

        let response = provider
            .chat(ChatRequest {
                provider: Some("openai-test".into()),
                model: Some("requested-model".into()),
                messages: vec![
                    ChatMessage::system("system contract"),
                    ChatMessage::user("user contract"),
                    ChatMessage::assistant("assistant contract"),
                ],
                temperature: Some(0.25),
                max_tokens: Some(77),
                use_memory: false,
            })
            .await
            .unwrap();

        assert_eq!(response.provider, "openai-test");
        assert_eq!(response.model.as_deref(), Some("requested-model"));
        assert_eq!(response.content, "contract-ok");
        let usage = response.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (11, 3));

        let request = request.await.unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(
            request.header("authorization"),
            Some("Bearer secret-test-key")
        );
        let payload = request.json();
        assert_eq!(payload["model"], "requested-model");
        assert_eq!(payload["temperature"], 0.25);
        assert_eq!(payload["max_tokens"], 77);
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][1]["role"], "user");
        assert_eq!(payload["messages"][2]["role"], "assistant");
    }

    #[tokio::test]
    async fn vllm_model_contract_uses_openai_endpoint_and_filters_empty_ids() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"data":[{"id":"model-a"},{"id":"  "},{"id":"model-b"}]}"#,
        )
        .await;
        let provider = OpenAICompatibleProvider::vllm(base_url, None, None).unwrap();

        let models = provider.list_models().await.unwrap();
        assert_eq!(
            models.into_iter().map(|model| model.id).collect::<Vec<_>>(),
            ["model-a", "model-b"]
        );
        let request = request.await.unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/models");
        assert!(request.header("authorization").is_none());
    }

    #[tokio::test]
    async fn provider_http_failures_keep_retry_classification_and_retry_after() {
        let (base_url, request) = serve_once(
            "429 Too Many Requests",
            &[("Retry-After", "9")],
            "rate limited",
        )
        .await;
        let provider =
            OpenAICompatibleProvider::new("test", "openai", base_url, None, Some("m".into()))
                .unwrap();

        let error = provider
            .chat(ChatRequest::from_prompt("hello"))
            .await
            .unwrap_err();
        let provider_error = provider_error(&error).expect("typed provider error");
        assert_eq!(provider_error.kind, ProviderErrorKind::RateLimited);
        assert_eq!(provider_error.retry_after, Some(Duration::from_secs(9)));
        assert_eq!(request.await.unwrap().path, "/chat/completions");
    }
}
