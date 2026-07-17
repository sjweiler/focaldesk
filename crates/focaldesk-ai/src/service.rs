use anyhow::{Context, Result, anyhow};
use focaldesk_memory::{
    EmbeddingProvider, MemoryId, MemoryService, MemoryStore, OllamaEmbeddingProvider, SearchHit,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::permissions::authorize_ai_chat;
use crate::provider::AiProvider;
use crate::providers::{
    AnthropicProvider, LocalCpuProvider, OllamaProvider, OpenAICompatibleProvider,
};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo, ProviderModelInfo,
};

/// Memories relevant to a chat prompt are capped here so the recalled
/// context doesn't dwarf the actual conversation.
const CHAT_RECALL_TOP_K: usize = 5;

pub struct AiService {
    providers: BTreeMap<String, Arc<dyn AiProvider>>,
    default_provider: String,
    request_timeout: Duration,
    concurrency: Arc<Semaphore>,
    active_requests: Arc<AtomicUsize>,
    memory: Option<MemoryService>,
}

impl AiService {
    pub fn new(default_provider: impl Into<String>) -> Self {
        Self {
            providers: BTreeMap::new(),
            default_provider: default_provider.into(),
            request_timeout: Duration::from_secs(120),
            concurrency: Arc::new(Semaphore::new(2)),
            active_requests: Arc::new(AtomicUsize::new(0)),
            memory: None,
        }
    }

    pub fn from_env() -> Result<Self> {
        let default_provider =
            std::env::var("FOCALDESK_AI_PROVIDER").unwrap_or_else(|_| "ollama".into());
        let mut service = Self::new(default_provider);

        let ollama_base = std::env::var("FOCALDESK_OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let ollama_model = std::env::var("FOCALDESK_OLLAMA_MODEL").ok();
        service.register(Arc::new(OllamaProvider::new(
            ollama_base.clone(),
            ollama_model,
        )?));

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

        if std::env::var("FOCALDESK_MEMORY_ENABLED").as_deref() != Ok("0") {
            match build_memory_service(&ollama_base) {
                Ok(memory) => service.memory = Some(memory),
                Err(err) => warn!(
                    target: "focaldesk.ai",
                    error = %err,
                    "AI memory store disabled: failed to initialize"
                ),
            }
        }

        Ok(service)
    }

    /// Attaches a memory store built elsewhere (tests, alternate embedding
    /// backends) instead of the one `from_env` would construct.
    pub fn with_memory(mut self, memory: MemoryService) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn has_memory(&self) -> bool {
        self.memory.is_some()
    }

    pub async fn remember(&self, text: String, metadata: serde_json::Value) -> Result<MemoryId> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        memory.remember_text(text, metadata).await
    }

    pub async fn recall(&self, query: String, top_k: usize) -> Result<Vec<SearchHit>> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        memory.recall_similar(&query, top_k).await
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

    pub async fn provider_models(&self, provider_id: &str) -> Result<Vec<ProviderModelInfo>> {
        let provider = self
            .providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown AI provider: {provider_id}"))?;
        provider.list_models().await
    }

    pub fn default_provider(&self) -> &str {
        &self.default_provider
    }

    pub fn status(&self) -> crate::types::AiDaemonStatus {
        crate::types::AiDaemonStatus {
            active_requests: self.active_requests.load(Ordering::Relaxed) as u32,
            default_provider: self.default_provider.clone(),
            provider_count: self.providers.len(),
        }
    }

    pub async fn chat(&self, mut request: ChatRequest) -> Result<ChatResponse> {
        let provider_id = request
            .provider
            .clone()
            .unwrap_or_else(|| self.default_provider.clone());
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown AI provider: {provider_id}"))?;

        if request.use_memory {
            self.augment_with_memory(&mut request).await;
        }

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
        self.active_requests.fetch_add(1, Ordering::Relaxed);
        struct ActiveRequestGuard<'a> {
            counter: &'a AtomicUsize,
        }
        impl Drop for ActiveRequestGuard<'_> {
            fn drop(&mut self) {
                self.counter.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let _active_guard = ActiveRequestGuard {
            counter: &self.active_requests,
        };

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

    /// Recalls memories relevant to the latest user turn and prepends them
    /// as a system message. Recall failures are logged and swallowed rather
    /// than failing the chat request — memory is a best-effort enhancement,
    /// not a hard dependency for chatting.
    async fn augment_with_memory(&self, request: &mut ChatRequest) {
        let Some(memory) = &self.memory else {
            warn!(
                target: "focaldesk.ai",
                "chat requested use_memory but no memory store is configured"
            );
            return;
        };

        let Some(latest_user) = request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, ChatRole::User))
        else {
            return;
        };

        match memory
            .recall_similar(&latest_user.content, CHAT_RECALL_TOP_K)
            .await
        {
            Ok(hits) if !hits.is_empty() => {
                let context = hits
                    .iter()
                    .map(|hit| format!("- {}", hit.record.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                request.messages.insert(
                    0,
                    ChatMessage::system(format!(
                        "Relevant memory from prior conversations:\n{context}"
                    )),
                );
            }
            Ok(_) => {}
            Err(err) => warn!(
                target: "focaldesk.ai",
                error = %err,
                "memory recall failed, continuing chat without it"
            ),
        }
    }
}

/// Builds the default memory backend: a local sqlite-vec file embedding text
/// via the same Ollama instance used for chat, at
/// `$FOCALDESK_OLLAMA_EMBED_MODEL` (default `nomic-embed-text`, 768 dims).
fn build_memory_service(ollama_base: &str) -> Result<MemoryService> {
    let model =
        std::env::var("FOCALDESK_OLLAMA_EMBED_MODEL").unwrap_or_else(|_| "nomic-embed-text".into());
    let dimension: usize = std::env::var("FOCALDESK_OLLAMA_EMBED_DIM")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(768);

    let store =
        MemoryStore::open_default(dimension).context("failed to open default AI memory store")?;
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(
        OllamaEmbeddingProvider::new(ollama_base.to_string(), model.clone(), dimension)
            .context("failed to build Ollama embedding provider")?,
    );

    info!(
        target: "focaldesk.ai",
        model = %model,
        dimension,
        "AI memory store enabled"
    );

    Ok(MemoryService::new(store, embedder))
}

fn build_prompt_message(request: &ChatRequest, provider_id: &str) -> String {
    let model = request.model.as_deref().unwrap_or("default model");
    let preview = request
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, ChatRole::User))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    #[test]
    fn permission_preview_uses_latest_user_turn() {
        let mut request = ChatRequest::from_prompt("historical prompt");
        request
            .messages
            .push(ChatMessage::assistant("historical reply"));
        request.messages.push(ChatMessage::user("current prompt"));

        let message = build_prompt_message(&request, "test-provider");

        assert!(message.contains("Preview: current prompt"));
        assert!(!message.contains("Preview: historical prompt"));
    }
}
