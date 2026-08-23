use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::{AiProvider, ProviderError, retry_after};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo, ProviderModelInfo, TokenUsage,
};

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    default_model: Option<String>,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String, default_model: Option<String>) -> Result<Self> {
        Self::with_base_url(api_key, "https://api.anthropic.com", default_model)
    }

    fn with_base_url(
        api_key: String,
        base_url: impl Into<String>,
        default_model: Option<String>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .context("failed to build Anthropic HTTP client")?;

        Ok(Self {
            api_key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
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
            base_url: Some(self.base_url.clone()),
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
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                ProviderError::from_transport("Anthropic chat request failed", error)
            })?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.map_err(|error| {
            ProviderError::from_transport("failed to read Anthropic response body", error)
        })?;

        if !status.is_success() {
            return Err(ProviderError::from_http("Anthropic", status, &body)
                .with_retry_after(retry_after)
                .into());
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
            usage: decoded.usage.map(|usage| TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }),
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
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::serve_once;

    #[tokio::test]
    async fn anthropic_contract_extracts_system_messages_and_preserves_usage() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"content":[{"type":"text","text":"hello "},{"type":"tool_use","id":"ignored"},{"type":"text","text":"world"}],"usage":{"input_tokens":13,"output_tokens":4}}"#,
        )
        .await;
        let provider = AnthropicProvider::with_base_url(
            "anthropic-secret".into(),
            base_url,
            Some("claude-test".into()),
        )
        .unwrap();

        let response = provider
            .chat(ChatRequest {
                provider: Some("anthropic".into()),
                model: None,
                messages: vec![
                    ChatMessage::system("first system"),
                    ChatMessage::system("second system"),
                    ChatMessage::user("question"),
                    ChatMessage::assistant("prior answer"),
                ],
                temperature: Some(0.1),
                max_tokens: Some(88),
                use_memory: false,
            })
            .await
            .unwrap();

        assert_eq!(response.content, "hello world");
        assert_eq!(response.model.as_deref(), Some("claude-test"));
        let usage = response.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (13, 4));

        let request = request.await.unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(request.header("x-api-key"), Some("anthropic-secret"));
        assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
        let payload = request.json();
        assert_eq!(payload["model"], "claude-test");
        assert_eq!(payload["system"], "first system\n\nsecond system");
        assert_eq!(payload["messages"].as_array().unwrap().len(), 2);
        assert_eq!(payload["messages"][0]["role"], "user");
        assert_eq!(payload["messages"][1]["role"], "assistant");
        assert_eq!(payload["max_tokens"], 88);
    }

    #[tokio::test]
    async fn anthropic_contract_rejects_success_without_text_content() {
        let (base_url, _request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"content":[{"type":"tool_use","id":"ignored"}]}"#,
        )
        .await;
        let provider =
            AnthropicProvider::with_base_url("key".into(), base_url, Some("model".into())).unwrap();

        let error = provider
            .chat(ChatRequest::from_prompt("hello"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("did not include text content"));
    }
}
