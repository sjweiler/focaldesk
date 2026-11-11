use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::provider::{retry_after, AiProvider, ProviderError};
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
    keep_alive: String,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, default_model: Option<String>) -> Result<Self> {
        let keep_alive =
            std::env::var("FOCALDESK_OLLAMA_KEEP_ALIVE").unwrap_or_else(|_| "20m".into());
        validate_keep_alive(&keep_alive)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build Ollama HTTP client")?;

        Ok(Self {
            id: "ollama".into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            default_model,
            client,
            keep_alive,
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

    async fn warm_up(&self) -> Result<()> {
        if self.keep_alive == "0" {
            return Ok(());
        }
        let model = self.resolve_model(None).await?;
        let response = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&serde_json::json!({"model":model,"stream":false,"keep_alive":self.keep_alive}))
            .send()
            .await
            .map_err(|e| ProviderError::from_transport("Ollama warm-up request failed", e))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::from_http("Ollama warm-up", status, &body).into());
        }
        let value: serde_json::Value = response
            .json()
            .await
            .context("invalid Ollama warm-up response")?;
        if value.get("error").is_some() {
            bail!("Ollama warm-up returned an error");
        }
        if value.get("done").and_then(serde_json::Value::as_bool) != Some(true) {
            bail!("Ollama warm-up did not complete");
        }
        Ok(())
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
            .filter(OllamaModelInfo::supports_chat)
            .map(|model| ProviderModelInfo { id: model.name })
            .filter(|model| !model.id.trim().is_empty())
            .collect())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.complete(request, false).await
    }
    async fn chat_json(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.complete(request, true).await
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
            keep_alive: self.keep_alive.clone(),
            messages: request.messages.iter().map(OllamaMessage::from).collect(),
            stream: true,
            format: None,
            think: None,
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
        let mut timings = None;
        while let Some(chunk) = stream.next().await {
            pending.extend_from_slice(&chunk.map_err(|error| {
                ProviderError::from_transport("read Ollama stream chunk", error)
            })?);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                if newline > OLLAMA_MAX_STREAM_LINE_BYTES {
                    bail!("Ollama stream event exceeds {OLLAMA_MAX_STREAM_LINE_BYTES} bytes");
                }
                let line = pending.drain(..=newline).collect::<Vec<_>>();
                consume_stream_line(
                    &line,
                    &mut content,
                    &mut usage,
                    &mut has_usage,
                    &mut timings,
                    &deltas,
                )
                .await?;
            }
            if pending.len() > OLLAMA_MAX_STREAM_LINE_BYTES {
                bail!("Ollama stream event exceeds {OLLAMA_MAX_STREAM_LINE_BYTES} bytes");
            }
        }
        if !pending.iter().all(u8::is_ascii_whitespace) {
            consume_stream_line(
                &pending,
                &mut content,
                &mut usage,
                &mut has_usage,
                &mut timings,
                &deltas,
            )
            .await?;
        }
        if let Some(timings) = timings {
            timings.record(has_usage.then_some(usage.output_tokens));
        }
        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content,
            usage: has_usage.then_some(usage),
            citations: Vec::new(),
        })
    }
}

async fn consume_stream_line(
    line: &[u8],
    content: &mut String,
    usage: &mut TokenUsage,
    has_usage: &mut bool,
    timings: &mut Option<OllamaTimings>,
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
    if event.done && timings.is_none() {
        *timings = Some(event.timings);
    }
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
    keep_alive: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

impl From<&ChatMessage> for OllamaMessage {
    fn from(message: &ChatMessage) -> Self {
        Self {
            role: message.role.as_str(),
            content: message.content.clone(),
            images: message
                .images
                .iter()
                .map(|image| {
                    image
                        .split_once(',')
                        .map(|(_, data)| data)
                        .unwrap_or(image)
                        .to_string()
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    #[serde(flatten)]
    timings: OllamaTimings,
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
    #[serde(default)]
    done: bool,
    #[serde(flatten)]
    timings: OllamaTimings,
    message: OllamaResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct OllamaTimings {
    #[serde(default)]
    load_duration: Option<u64>,
    #[serde(default)]
    prompt_eval_duration: Option<u64>,
    #[serde(default)]
    eval_duration: Option<u64>,
}
impl OllamaTimings {
    fn record(&self, tokens: Option<u64>) {
        focaldesk_logging::metrics::record_ollama_timings(
            self.load_duration,
            self.prompt_eval_duration,
            self.eval_duration,
            tokens,
        );
    }
}

fn validate_keep_alive(value: &str) -> Result<()> {
    if matches!(value, "0" | "-1") {
        return Ok(());
    }
    let end = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let amount = value[..end].parse::<u64>();
    if !amount.is_ok_and(|n| n > 0) || !matches!(&value[end..], "ms" | "s" | "m" | "h") {
        bail!(
            "FOCALDESK_OLLAMA_KEEP_ALIVE must be 0, -1, or an integer duration such as 20m or 1h"
        );
    }
    Ok(())
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
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

impl OllamaModelInfo {
    fn supports_chat(&self) -> bool {
        self.capabilities.as_ref().is_none_or(|capabilities| {
            capabilities
                .iter()
                .any(|capability| capability == "completion")
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn keep_alive_accepts_finite_durations_and_explicit_disable_or_pin() {
        for value in ["20m", "1h", "30s", "500ms", "0", "-1"] {
            assert!(validate_keep_alive(value).is_ok());
        }
        for value in ["", "forever", "-2", "20", "1.5m", "20m garbage"] {
            assert!(validate_keep_alive(value).is_err());
        }
    }

    #[tokio::test]
    async fn warm_up_loads_only_the_selected_model_with_configured_residency() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"done":true}"#,
        )
        .await;
        let mut provider = OllamaProvider::new(base_url, Some("selected-model".into())).unwrap();
        provider.keep_alive = "20m".into();
        provider.warm_up().await.unwrap();
        let request = request.await.unwrap();
        assert_eq!(request.path, "/api/generate");
        let body = request.json();
        assert_eq!(body["model"], "selected-model");
        assert_eq!(body["keep_alive"], "20m");
        assert_eq!(body["stream"], false);
        assert!(body.get("prompt").is_none());
        assert!(body.get("messages").is_none());
    }

    #[tokio::test]
    async fn disabling_keep_alive_skips_warm_up_without_a_network_request() {
        let mut provider =
            OllamaProvider::new("http://127.0.0.1:1", Some("unused".into())).unwrap();
        provider.keep_alive = "0".into();
        provider.warm_up().await.unwrap();
    }

    #[tokio::test]
    async fn only_terminal_stream_event_supplies_timings_and_missing_fields_remain_unknown() {
        let (tx, _rx) = mpsc::channel(2);
        let mut content = String::new();
        let mut usage = TokenUsage::default();
        let mut has_usage = false;
        let mut timings = None;
        consume_stream_line(
            br#"{"message":{"content":""},"done":false,"load_duration":999}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &mut timings,
            &tx,
        )
        .await
        .unwrap();
        assert!(timings.is_none());
        consume_stream_line(br#"{"message":{"content":""},"done":true,"load_duration":1000000,"prompt_eval_duration":0,"eval_duration":3000000,"eval_count":12}"#, &mut content,&mut usage,&mut has_usage,&mut timings,&tx).await.unwrap();
        let terminal = timings.as_ref().unwrap();
        assert_eq!(terminal.load_duration, Some(1_000_000));
        assert_eq!(terminal.prompt_eval_duration, Some(0));
        assert_eq!(terminal.eval_duration, Some(3_000_000));
        assert_eq!(usage.output_tokens, 12);
        consume_stream_line(
            br#"{"message":{"content":""},"done":true,"load_duration":9}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &mut timings,
            &tx,
        )
        .await
        .unwrap();
        assert_eq!(timings.as_ref().unwrap().load_duration, Some(1_000_000));
        let legacy: OllamaChatResponse =
            serde_json::from_str(r#"{"message":{"content":"ok"}}"#).unwrap();
        assert!(legacy.timings.load_duration.is_none());
        assert!(legacy.timings.eval_duration.is_none());
    }

    #[test]
    fn image_attachment_uses_provider_wire_format() {
        let image = "data:image/png;base64,aW1hZ2U=";
        let mut message = ChatMessage::user("Describe this image");
        message.images.push(image.into());
        let value = serde_json::to_value(OllamaMessage::from(&message)).unwrap();
        assert_eq!(value["images"][0], "aW1hZ2U=");
    }
    use super::*;
    use crate::test_support::serve_once;

    #[tokio::test]
    async fn managed_planning_requests_json_without_thinking_and_preserves_telemetry() {
        use crate::managed_provider::{ManagedProvider, RetryPolicy};
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        let body=serde_json::json!({"message":{"content":"{\"steps\":[],\"answer\":\"done\"}"},"prompt_eval_count":8,"eval_count":3}).to_string();
        let (base_url, request) =
            serve_once("200 OK", &[("Content-Type", "application/json")], body).await;
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let provider = ManagedProvider::new(
            Arc::new(OllamaProvider::new(base_url, Some("planner-test".into())).unwrap()),
            telemetry.clone(),
            RetryPolicy::default(),
        );
        let response = provider
            .chat_json(ChatRequest::from_prompt("Return a plan"))
            .await
            .unwrap();
        let plan = crate::Planner::parse(&response.content, &[]).unwrap();
        assert_eq!(plan.answer.as_deref(), Some("done"));
        let payload = request.await.unwrap().json();
        assert_eq!(payload["format"], "json");
        assert_eq!(payload["think"], false);
        assert_eq!(telemetry.lock().unwrap()["ollama"].successes, 1);
    }

    #[tokio::test]
    async fn ollama_model_list_excludes_embedding_only_models() {
        let (base_url, request) = serve_once(
            "200 OK",
            &[("Content-Type", "application/json")],
            concat!(
                "{\"models\":[",
                "{\"name\":\"nomic-embed-text:latest\",\"capabilities\":[\"embedding\"]},",
                "{\"name\":\"llama3.1:latest\",\"capabilities\":[\"completion\",\"tools\"]},",
                "{\"name\":\"legacy-chat-model\"}",
                "]}"
            ),
        )
        .await;
        let provider = OllamaProvider::new(base_url, None).unwrap();

        let models = provider.list_models().await.unwrap();
        assert_eq!(
            models.into_iter().map(|model| model.id).collect::<Vec<_>>(),
            vec!["llama3.1:latest", "legacy-chat-model"]
        );
        assert_eq!(request.await.unwrap().path, "/api/tags");
    }

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
                memory_sources: None,
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
        assert_eq!(payload["keep_alive"], provider.keep_alive);
        assert!(payload.get("format").is_none());
        assert!(payload.get("think").is_none());
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
        let payload = request.await.unwrap().json();
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["keep_alive"], provider.keep_alive);
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
        assert!(error
            .to_string()
            .contains("failed to parse Ollama stream event"));
    }

    #[tokio::test]
    async fn parses_ollama_stream_events_and_emits_deltas() {
        let (tx, mut rx) = mpsc::channel(2);
        let mut content = String::new();
        let mut usage = TokenUsage::default();
        let mut has_usage = false;
        let mut timings = None;
        consume_stream_line(
            br#"{"message":{"content":"hel"},"done":false}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &mut timings,
            &tx,
        )
        .await
        .unwrap();
        consume_stream_line(
            br#"{"message":{"content":"lo"},"done":true,"prompt_eval_count":7,"eval_count":2}"#,
            &mut content,
            &mut usage,
            &mut has_usage,
            &mut timings,
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

impl OllamaProvider {
    async fn complete(&self, request: ChatRequest, json_mode: bool) -> Result<ChatResponse> {
        let model = self.resolve_model(request.model.clone()).await?;

        if request.messages.is_empty() {
            bail!("chat request must include at least one message");
        }

        let payload = OllamaChatRequest {
            model: model.clone(),
            keep_alive: self.keep_alive.clone(),
            messages: request.messages.iter().map(OllamaMessage::from).collect(),
            stream: false,
            format: if json_mode { Some("json") } else { None },
            think: if json_mode { Some(false) } else { None },
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
        decoded.timings.record(decoded.eval_count);

        Ok(ChatResponse {
            provider: self.id.clone(),
            model: Some(model),
            content: decoded.message.content,
            usage: ollama_usage(decoded.prompt_eval_count, decoded.eval_count),
            citations: Vec::new(),
        })
    }
}
