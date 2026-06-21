use anyhow::{Context, Result, anyhow};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::permissions::authorize_ai_chat;
use crate::provider::AiProvider;
use crate::providers::{
    AnthropicProvider, LocalCpuProvider, OllamaProvider, OpenAICompatibleProvider,
};
use crate::types::{ChatRequest, ChatResponse, ProviderInfo};

pub struct AiService {
    providers: BTreeMap<String, Arc<dyn AiProvider>>,
    default_provider: String,
    request_timeout: Duration,
    concurrency: Arc<Semaphore>,
}

impl AiService {
    pub fn new(default_provider: impl Into<String>) -> Self {
        Self {
            providers: BTreeMap::new(),
            default_provider: default_provider.into(),
            request_timeout: Duration::from_secs(120),
            concurrency: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn from_env() -> Result<Self> {
        let default_provider =
            std::env::var("FOCALDESK_AI_PROVIDER").unwrap_or_else(|_| "ollama".into());
        let mut service = Self::new(default_provider);

        let ollama_base = std::env::var("FOCALDESK_OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let ollama_model = std::env::var("FOCALDESK_OLLAMA_MODEL").ok();
        service.register(Arc::new(OllamaProvider::new(ollama_base, ollama_model)?));

        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            service.register(Arc::new(OpenAICompatibleProvider::openai(
                api_key,
                std::env::var("FOCALDESK_OPENAI_MODEL").ok(),
            )?));
        }

        if let Ok(base_url) = std::env::var("FOCALDESK_VLLM_BASE_URL") {
            service.register(Arc::new(OpenAICompatibleProvider::vllm(
                base_url,
                std::env::var("FOCALDESK_VLLM_API_KEY").ok(),
                std::env::var("FOCALDESK_VLLM_MODEL").ok(),
            )?));
        }

        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            service.register(Arc::new(AnthropicProvider::new(
                api_key,
                std::env::var("FOCALDESK_ANTHROPIC_MODEL").ok(),
            )?));
        }

        service.register(Arc::new(LocalCpuProvider));

        Ok(service)
    }

    pub fn register(&mut self, provider: Arc<dyn AiProvider>) {
        let id = provider.info().id;
        self.providers.insert(id, provider);
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.providers
            .values()
            .map(|provider| provider.info())
            .collect()
    }

    pub fn default_provider(&self) -> &str {
        &self.default_provider
    }

    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let provider_id = request
            .provider
            .clone()
            .unwrap_or_else(|| self.default_provider.clone());
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown AI provider: {provider_id}"))?;

        info!(
            target: "focaldesk.ai",
            provider = %provider_id,
            model = request.model.as_deref().unwrap_or("-"),
            messages = request.messages.len(),
            "AI chat request received"
        );

        let prompt_title = format!("Allow AI chat from {provider_id}?");
        let prompt_message = build_prompt_message(&request, &provider_id);
        authorize_ai_chat(&prompt_title, &prompt_message, true)
            .with_context(|| format!("AI chat blocked for provider {provider_id}"))?;

        let _permit = self
            .concurrency
            .acquire()
            .await
            .context("AI request concurrency limiter closed")?;

        let started = std::time::Instant::now();
        let response = timeout(self.request_timeout, provider.chat(request))
            .await
            .with_context(|| format!("AI provider {provider_id} timed out"))??;

        info!(
            target: "focaldesk.ai",
            provider = %response.provider,
            model = response.model.as_deref().unwrap_or("-"),
            content_len = response.content.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "AI chat response completed"
        );

        if response.provider != provider_id {
            warn!(
                target: "focaldesk.ai",
                expected_provider = %provider_id,
                actual_provider = %response.provider,
                "AI provider returned a mismatched provider id"
            );
        }

        Ok(response)
    }
}

fn build_prompt_message(request: &ChatRequest, provider_id: &str) -> String {
    let model = request.model.as_deref().unwrap_or("default model");
    let preview = request
        .messages
        .iter()
        .find(|message| matches!(message.role, crate::types::ChatRole::User))
        .map(|message| truncate_preview(&message.content, 160))
        .unwrap_or_else(|| "no user message preview available".to_string());

    format!(
        "Provider: {provider_id}\nModel: {model}\nMessages: {}\nPreview: {preview}",
        request.messages.len()
    )
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut preview = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}
