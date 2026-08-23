use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::provider::{AiProvider, ProviderError, retry_after};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo, TokenUsage,
};

const OLLAMA_MAX_STREAM_LINE_BYTES: usize = 512 * 1024;

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
            .map_err(|error| {
                ProviderError::from_transport("Ollama model list request failed", error)
            })?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.map_err(|error| {
            ProviderError::from_transport("failed to read Ollama model list response body", error)
        })?;

        if !status.is_success() {
            return Err(ProviderError::from_http("Ollama", status, &body)
                .with_retry_after(retry_after)
                .into());
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
            .map_err(|error| ProviderError::from_transport("Ollama chat request failed", error))?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.map_err(|error| {
            ProviderError::from_transport("failed to read Ollama response body", error)
        })?;

        if !status.is_success() {
            return Err(ProviderError::from_http("Ollama", status, &body)
                .with_retry_after(retry_after)
                .into());
        }

        let decoded: OllamaChatResponse =
            serde_json::from_str(&body).context("failed to parse Ollama chat response")?;

        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content: decoded.message.content,
            usage: ollama_usage(decoded.prompt_eval_count, decoded.eval_count),
        })
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        deltas: mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        let model = self.resolve_model(request.model.clone()).await?;
        if request.messages.is_empty() {
            bail!("chat request must include at least one message");
        }
        let payload = OllamaChatRequest {
            model: model.clone(),
            messages: request.messages.iter().map(OllamaMessage::from).collect(),
            stream: true,
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
            .map_err(|error| {
                ProviderError::from_transport("Ollama streaming chat request failed", error)
            })?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::from_http("Ollama", status, &body)
                .with_retry_after(retry_after)
                .into());
        }

        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        let mut content = String::new();
        let mut usage = TokenUsage::default();
        let mut has_usage = false;
        while let Some(chunk) = stream.next().await {
            pending.extend_from_slice(&chunk.map_err(|error| {
                ProviderError::from_transport("read Ollama stream chunk", error)
            })?);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                if newline > OLLAMA_MAX_STREAM_LINE_BYTES {
                    bail!("Ollama stream event exceeds {OLLAMA_MAX_STREAM_LINE_BYTES} bytes");
                }
                let line = pending.drain(..=newline).collect::<Vec<_>>();
                consume_stream_line(&line, &mut content, &mut usage, &mut has_usage, &deltas)
                    .await?;
            }
            if pending.len() > OLLAMA_MAX_STREAM_LINE_BYTES {
                bail!("Ollama stream event exceeds {OLLAMA_MAX_STREAM_LINE_BYTES} bytes");
            }
        }
        if !pending.iter().all(u8::is_ascii_whitespace) {
            consume_stream_line(&pending, &mut content, &mut usage, &mut has_usage, &deltas)
                .await?;
        }
        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content,
            usage: has_usage.then_some(usage),
        })
    }
}

async fn consume_stream_line(
    line: &[u8],
    content: &mut String,
    usage: &mut TokenUsage,
    has_usage: &mut bool,
    deltas: &mpsc::Sender<String>,
) -> Result<()> {
    let line = std::str::from_utf8(line)
        .context("Ollama stream contained invalid UTF-8")?
        .trim();
    if line.is_empty() {
        return Ok(());
    }
    let event: OllamaStreamResponse =
        serde_json::from_str(line).context("failed to parse Ollama stream event")?;
    if let Some(input_tokens) = event.prompt_eval_count {
        usage.input_tokens = input_tokens;
        *has_usage = true;
    }
    if let Some(output_tokens) = event.eval_count {
        usage.output_tokens = output_tokens;
        *has_usage = true;
    }
    if !event.message.content.is_empty() {
        content.push_str(&event.message.content);
        deltas
            .send(event.message.content)
            .await
            .map_err(|_| anyhow!("Ollama stream consumer disconnected"))?;
    }
    Ok(())
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
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaStreamResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

fn ollama_usage(input_tokens: Option<u64>, output_tokens: Option<u64>) -> Option<TokenUsage> {
    (input_tokens.is_some() || output_tokens.is_some()).then_some(TokenUsage {
        input_tokens: input_tokens.unwrap_or(0),
        output_tokens: output_tokens.unwrap_or(0),
    })
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::serve_once;

    #[tokio::test]
    async fn ollama_chat_contract_preserves_options_roles_and_usage() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"message":{"content":"ollama-ok"},"prompt_eval_count":8,"eval_count":3}"#,
        )
        .await;
        let provider = OllamaProvider::new(base_url, Some("llama-test".into())).unwrap();

        let response = provider
            .chat(ChatRequest {
                provider: Some("ollama".into()),
                model: None,
                messages: vec![
                    ChatMessage::system("system"),
                    ChatMessage::user("user"),
                    ChatMessage::assistant("assistant"),
                ],
                temperature: Some(0.5),
                max_tokens: Some(64),
                use_memory: false,
            })
            .await
            .unwrap();

        assert_eq!(response.content, "ollama-ok");
        let usage = response.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (8, 3));
        let request = request.await.unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/chat");
        let payload = request.json();
        assert_eq!(payload["model"], "llama-test");
        assert_eq!(payload["stream"], false);
        assert_eq!(payload["options"]["temperature"], 0.5);
        assert_eq!(payload["options"]["num_predict"], 64);
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][2]["role"], "assistant");
    }

    #[tokio::test]
    async fn ollama_stream_contract_emits_ordered_deltas_and_terminal_usage() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/x-ndjson")],
            concat!(
                "{\"message\":{\"content\":\"one \"},\"done\":false}\n",
                "{\"message\":{\"content\":\"two\"},\"done\":true,\"prompt_eval_count\":5,\"eval_count\":2}\n"
            ),
        )
        .await;
        let provider = OllamaProvider::new(base_url, Some("stream-model".into())).unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let response = provider
            .chat_stream(ChatRequest::from_prompt("stream"), tx)
            .await
            .unwrap();
        assert_eq!(rx.recv().await.as_deref(), Some("one "));
        assert_eq!(rx.recv().await.as_deref(), Some("two"));
        assert!(rx.recv().await.is_none());
        assert_eq!(response.content, "one two");
        let usage = response.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (5, 2));
        assert_eq!(request.await.unwrap().json()["stream"], true);
    }

    #[tokio::test]
    async fn ollama_stream_contract_rejects_malformed_events() {
        let (base_url, _request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/x-ndjson")],
            "not-json\n",
        )
        .await;
        let provider = OllamaProvider::new(base_url, Some("stream-model".into())).unwrap();
        let (tx, _rx) = mpsc::channel(1);

        let error = provider
            .chat_stream(ChatRequest::from_prompt("stream"), tx)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to parse Ollama stream event")
        );
    }

    #[tokio::test]
    async fn parses_ollama_stream_events_and_emits_deltas() {
        let (tx, mut rx) = mpsc::channel(2);
        let mut content = String::new();
        let mut usage = TokenUsage::default();
        let mut has_usage = false;
        consume_stream_line(
            br#"{"message":{"content":"hel"},"done":false}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &tx,
        )
        .await
        .unwrap();
        consume_stream_line(
            br#"{"message":{"content":"lo"},"done":true,"prompt_eval_count":7,"eval_count":2}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(content, "hello");
        assert_eq!(rx.recv().await.as_deref(), Some("hel"));
        assert_eq!(rx.recv().await.as_deref(), Some("lo"));
        assert!(has_usage);
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 2);
    }
}
