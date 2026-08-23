use anyhow::{Context, Result, anyhow};
use focaldesk_memory::{
    EmbeddingProvider, MemoryId, MemoryPolicy, MemoryService, MemoryStatus, MemoryStore,
    OllamaEmbeddingProvider, SearchHit,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::managed_provider::{ManagedProvider, RetryPolicy};
use crate::permissions::{authorize_ai_chat, confirm_ai_action};
use crate::provider::AiProvider;
use crate::providers::{AnthropicProvider, OllamaProvider, OpenAICompatibleProvider};
use crate::types::{
    AiStreamEvent, ChatMessage, ChatRequest, ChatResponse, ChatRole, ProviderInfo,
    ProviderModelInfo,
};
use crate::{
    Agent, AgentActionResponse, AgentConfirmation, AgentProposedAction, AgentRequest,
    AgentResponse, AgentToolExecutor,
};

const AI_MAX_STREAM_CONTENT_BYTES: usize = 512 * 1024;

/// Memories relevant to a chat prompt are capped here so the recalled
/// context doesn't dwarf the actual conversation.
const CHAT_RECALL_TOP_K: usize = 5;
const AGENT_ACTION_TTL: Duration = Duration::from_secs(120);
const MAX_PENDING_AGENT_ACTIONS: usize = 64;

#[derive(Debug, Clone)]
struct PendingAgentAction {
    action: AgentProposedAction,
    expires_at: std::time::Instant,
}

struct StreamCancellationGuard {
    request_id: String,
    registry: Arc<Mutex<BTreeMap<String, watch::Sender<bool>>>>,
}

impl Drop for StreamCancellationGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(&self.request_id);
        }
    }
}

pub struct AiService {
    providers: BTreeMap<String, Arc<dyn AiProvider>>,
    default_provider: String,
    request_timeout: Duration,
    concurrency: Arc<Semaphore>,
    active_requests: Arc<AtomicUsize>,
    pending_permissions: Arc<AtomicUsize>,
    memory: Option<MemoryService>,
    tool_executor: Option<Arc<dyn AgentToolExecutor>>,
    pending_agent_actions: Mutex<BTreeMap<String, PendingAgentAction>>,
    stream_cancellations: Arc<Mutex<BTreeMap<String, watch::Sender<bool>>>>,
    provider_telemetry: Arc<Mutex<BTreeMap<String, crate::types::ProviderTelemetry>>>,
}

struct ActivityGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> ActivityGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

impl AiService {
    pub fn new(default_provider: impl Into<String>) -> Self {
        Self {
            providers: BTreeMap::new(),
            default_provider: default_provider.into(),
            request_timeout: Duration::from_secs(120),
            concurrency: Arc::new(Semaphore::new(2)),
            active_requests: Arc::new(AtomicUsize::new(0)),
            pending_permissions: Arc::new(AtomicUsize::new(0)),
            memory: None,
            tool_executor: None,
            pending_agent_actions: Mutex::new(BTreeMap::new()),
            stream_cancellations: Arc::new(Mutex::new(BTreeMap::new())),
            provider_telemetry: Arc::new(Mutex::new(BTreeMap::new())),
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

        if let Some(api_key) = credential("ai/openai-api-key", "OPENAI_API_KEY") {
            service.register(Arc::new(OpenAICompatibleProvider::openai(
                api_key.to_string(),
                std::env::var("FOCALDESK_OPENAI_MODEL").ok(),
            )?));
        }

        if let Ok(base_url) = std::env::var("FOCALDESK_VLLM_BASE_URL") {
            service.register(Arc::new(OpenAICompatibleProvider::vllm(
                base_url,
                credential("ai/vllm-api-key", "FOCALDESK_VLLM_API_KEY").map(|key| key.to_string()),
                std::env::var("FOCALDESK_VLLM_MODEL").ok(),
            )?));
        }

        if let Some(api_key) = credential("ai/anthropic-api-key", "ANTHROPIC_API_KEY") {
            service.register(Arc::new(AnthropicProvider::new(
                api_key.to_string(),
                std::env::var("FOCALDESK_ANTHROPIC_MODEL").ok(),
            )?));
        }

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

    pub fn with_tool_executor(mut self, executor: Arc<dyn AgentToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    pub fn has_agent_tools(&self) -> bool {
        self.tool_executor
            .as_ref()
            .is_some_and(|executor| !executor.tools().is_empty())
    }

    pub fn has_memory(&self) -> bool {
        self.memory.is_some()
    }

    pub async fn remember(&self, text: String, metadata: serde_json::Value) -> Result<MemoryId> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        authorize_ai_chat(
            "Allow AI memory storage?",
            &format!(
                "Store and embed this note: {}",
                truncate_preview(&text, 160)
            ),
            true,
        )
        .context("AI memory storage blocked")?;
        memory.remember_text(text, metadata).await
    }

    pub async fn recall(&self, query: String, top_k: usize) -> Result<Vec<SearchHit>> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        authorize_ai_chat(
            "Allow AI memory search?",
            &format!(
                "Embed this query and search local AI memory: {}",
                truncate_preview(&query, 160)
            ),
            true,
        )
        .context("AI memory search blocked")?;
        memory.recall_similar(&query, top_k).await
    }

    pub async fn forget(&self, id: MemoryId) -> Result<()> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        confirm_ai_action(
            "forget_memory",
            "Forget this AI memory?",
            &format!("Permanently delete AI memory record {id}. This cannot be undone."),
        )
        .context("AI memory deletion was not approved")?;
        memory.forget(id).await
    }

    pub async fn clear_memory(&self) -> Result<usize> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?;
        let status = memory.status().await?;
        confirm_ai_action(
            "clear_memory",
            "Clear all AI memory?",
            &format!(
                "Permanently delete all {} AI memory records. This cannot be undone.",
                status.entry_count
            ),
        )
        .context("bulk AI memory deletion was not approved")?;
        memory.clear().await
    }

    pub async fn memory_status(&self) -> Result<MemoryStatus> {
        self.memory
            .as_ref()
            .ok_or_else(|| anyhow!("AI memory store is not configured"))?
            .status()
            .await
    }

    pub fn register(&mut self, provider: Arc<dyn AiProvider>) {
        let id = provider.info().id;
        {
            let mut telemetry = self
                .provider_telemetry
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            telemetry
                .entry(id.clone())
                .or_insert_with(|| crate::types::ProviderTelemetry {
                    provider: id.clone(),
                    ..crate::types::ProviderTelemetry::default()
                });
        }
        let policy = RetryPolicy {
            overall_timeout: self.request_timeout,
            ..RetryPolicy::default()
        };
        self.providers.insert(
            id,
            Arc::new(ManagedProvider::new(
                provider,
                self.provider_telemetry.clone(),
                policy,
            )),
        );
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
        let provider_telemetry = self
            .provider_telemetry
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .cloned()
            .collect();
        crate::types::AiDaemonStatus {
            active_requests: self.active_requests.load(Ordering::Relaxed) as u32,
            pending_permissions: self.pending_permissions.load(Ordering::Relaxed) as u32,
            default_provider: self.default_provider.clone(),
            provider_count: self.providers.len(),
            provider_telemetry,
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

        info!(
            target: "focaldesk.ai",
            provider = %provider_id,
            model = request.model.as_deref().unwrap_or("-"),
            messages = request.messages.len(),
            "AI chat request received"
        );

        let prompt_title = format!("Allow AI chat from {provider_id}?");
        let prompt_message = build_prompt_message(&request, &provider_id);
        {
            let _permission_guard = ActivityGuard::new(&self.pending_permissions);
            authorize_ai_chat(&prompt_title, &prompt_message, true)
                .with_context(|| format!("AI chat blocked for provider {provider_id}"))?;
        }

        // Memory recall can contact the configured embedding endpoint. Keep it
        // after authorization so no part of a denied prompt leaves the service.
        if request.use_memory {
            self.augment_with_memory(&mut request).await;
        }

        let _permit = self
            .concurrency
            .acquire()
            .await
            .context("AI request concurrency limiter closed")?;
        let _active_guard = ActivityGuard::new(&self.active_requests);

        let started = std::time::Instant::now();
        let response = provider
            .chat(request)
            .await
            .with_context(|| format!("AI provider {provider_id} failed"))?;

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

    pub async fn chat_stream(
        &self,
        request_id: String,
        mut request: ChatRequest,
        events: mpsc::Sender<AiStreamEvent>,
    ) -> Result<()> {
        if request_id.is_empty() {
            return Err(anyhow!("stream request id must not be empty"));
        }
        let provider_id = request
            .provider
            .clone()
            .unwrap_or_else(|| self.default_provider.clone());
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown AI provider: {provider_id}"))?;

        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        {
            let mut registry = self
                .stream_cancellations
                .lock()
                .map_err(|_| anyhow!("AI cancellation registry is unavailable"))?;
            if registry.contains_key(&request_id) {
                return Err(anyhow!("duplicate streaming request id: {request_id}"));
            }
            registry.insert(request_id.clone(), cancel_tx);
        }
        let _cancellation_guard = StreamCancellationGuard {
            request_id: request_id.clone(),
            registry: self.stream_cancellations.clone(),
        };

        let prompt_title = format!("Allow streaming AI chat from {provider_id}?");
        let prompt_message = build_prompt_message(&request, &provider_id);
        {
            let _permission_guard = ActivityGuard::new(&self.pending_permissions);
            authorize_ai_chat(&prompt_title, &prompt_message, true)
                .with_context(|| format!("streaming AI chat blocked for provider {provider_id}"))?;
        }
        if *cancel_rx.borrow() {
            events
                .send(AiStreamEvent::Cancelled { request_id })
                .await
                .ok();
            return Ok(());
        }
        if request.use_memory {
            self.augment_with_memory(&mut request).await;
        }

        let permit = tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    events.send(AiStreamEvent::Cancelled {
                        request_id: request_id.clone(),
                    }).await.ok();
                    return Ok(());
                }
                return Err(anyhow!("AI stream cancellation channel closed"));
            }
            permit = self.concurrency.acquire() => {
                permit.context("AI request concurrency limiter closed")?
            }
        };
        let _permit = permit;
        let _active_guard = ActivityGuard::new(&self.active_requests);
        events
            .send(AiStreamEvent::Started {
                request_id: request_id.clone(),
                provider: provider_id.clone(),
                model: request
                    .model
                    .clone()
                    .or_else(|| provider.info().default_model),
            })
            .await
            .map_err(|_| anyhow!("AI stream consumer disconnected"))?;

        let (delta_tx, mut delta_rx) = mpsc::channel::<String>(32);
        let provider_future = provider.chat_stream(request, delta_tx);
        tokio::pin!(provider_future);
        let mut streamed_bytes = 0usize;
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        events.send(AiStreamEvent::Cancelled {
                            request_id: request_id.clone(),
                        }).await.ok();
                        return Ok(());
                    }
                }
                delta = delta_rx.recv() => {
                    if let Some(content) = delta {
                        streamed_bytes = streamed_bytes.saturating_add(content.len());
                        if streamed_bytes > AI_MAX_STREAM_CONTENT_BYTES {
                            events.send(AiStreamEvent::Failed {
                                request_id: request_id.clone(),
                                message: format!(
                                    "AI stream exceeds {AI_MAX_STREAM_CONTENT_BYTES} bytes"
                                ),
                            }).await.ok();
                            return Ok(());
                        }
                        events.send(AiStreamEvent::Delta {
                            request_id: request_id.clone(),
                            content,
                        }).await.map_err(|_| anyhow!("AI stream consumer disconnected"))?;
                    }
                }
                result = &mut provider_future => {
                    let response = result
                        .with_context(|| format!("streaming AI provider {provider_id} failed"))?;
                    if response.content.len() > AI_MAX_STREAM_CONTENT_BYTES {
                        events.send(AiStreamEvent::Failed {
                            request_id: request_id.clone(),
                            message: format!(
                                "AI stream exceeds {AI_MAX_STREAM_CONTENT_BYTES} bytes"
                            ),
                        }).await.ok();
                        return Ok(());
                    }
                    events.send(AiStreamEvent::Completed {
                        request_id: request_id.clone(),
                        response,
                    }).await.map_err(|_| anyhow!("AI stream consumer disconnected"))?;
                    return Ok(());
                }
            }
        }
    }

    pub fn cancel_stream(&self, request_id: &str) -> Result<bool> {
        let registry = self
            .stream_cancellations
            .lock()
            .map_err(|_| anyhow!("AI cancellation registry is unavailable"))?;
        let Some(sender) = registry.get(request_id) else {
            return Ok(false);
        };
        Ok(sender.send(true).is_ok())
    }

    pub async fn run_agent(&self, request: AgentRequest) -> Result<AgentResponse> {
        let objective = request.objective.trim();
        if objective.is_empty() {
            return Err(anyhow!("agent objective must not be empty"));
        }
        if objective.chars().count() > 4_000 {
            return Err(anyhow!("agent objective exceeds 4000 characters"));
        }
        let provider_id = request
            .provider
            .clone()
            .unwrap_or_else(|| self.default_provider.clone());
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown AI provider: {provider_id}"))?;
        let executor = self
            .tool_executor
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("AI agent tools are not configured"))?;

        let prompt_title = format!("Allow read-only AI desktop analysis from {provider_id}?");
        let prompt_message = format!(
            "The model may inspect bounded desktop metadata with read-only tools.\nObjective: {}",
            truncate_preview(objective, 160)
        );
        {
            let _permission_guard = ActivityGuard::new(&self.pending_permissions);
            authorize_ai_chat(&prompt_title, &prompt_message, true)
                .with_context(|| format!("AI agent blocked for provider {provider_id}"))?;
        }

        let _permit = self
            .concurrency
            .acquire()
            .await
            .context("AI request concurrency limiter closed")?;
        let _active_guard = ActivityGuard::new(&self.active_requests);
        let agent = Agent::new("focaldesk-read-only-agent".into());
        let mut response = timeout(
            self.request_timeout,
            agent.run(provider.as_ref(), executor.as_ref(), request),
        )
        .await
        .with_context(|| format!("AI agent using {provider_id} timed out"))??;

        if let Some(action) = response.proposed_action.clone() {
            let expires_at_unix = unix_now().saturating_add(AGENT_ACTION_TTL.as_secs());
            let pending = PendingAgentAction {
                action: action.clone(),
                expires_at: std::time::Instant::now() + AGENT_ACTION_TTL,
            };
            let mut plans = self
                .pending_agent_actions
                .lock()
                .map_err(|_| anyhow!("pending agent action store is unavailable"))?;
            let now = std::time::Instant::now();
            plans.retain(|expired_id, plan| {
                let keep = plan.expires_at > now;
                if !keep {
                    info!(
                        target: "focaldesk.ai",
                        plan_id = %expired_id,
                        tool = %plan.action.tool,
                        "AI agent action expired"
                    );
                }
                keep
            });
            if plans.len() >= MAX_PENDING_AGENT_ACTIONS {
                return Err(anyhow!("too many agent actions are awaiting confirmation"));
            }
            let plan_id = loop {
                let candidate = random_plan_id();
                if !plans.contains_key(&candidate) {
                    break candidate;
                }
            };
            plans.insert(plan_id.clone(), pending);
            response.confirmation = Some(AgentConfirmation {
                plan_id: plan_id.clone(),
                expires_at_unix,
                tool: action.tool.clone(),
                arguments: action.arguments.clone(),
            });
            info!(
                target: "focaldesk.ai",
                plan_id = %plan_id,
                tool = %action.tool,
                expires_at_unix,
                "AI agent action proposed"
            );
        }

        Ok(response)
    }

    pub async fn confirm_agent_action(
        &self,
        plan_id: String,
        approved: bool,
    ) -> Result<AgentActionResponse> {
        if plan_id.len() != 48 || !plan_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(anyhow!("invalid agent action plan id"));
        }
        let pending = {
            let mut plans = self
                .pending_agent_actions
                .lock()
                .map_err(|_| anyhow!("pending agent action store is unavailable"))?;
            plans.remove(&plan_id)
        }
        .ok_or_else(|| anyhow!("agent action plan is unknown, expired, or already resolved"))?;

        if pending.expires_at <= std::time::Instant::now() {
            info!(
                target: "focaldesk.ai",
                plan_id = %plan_id,
                tool = %pending.action.tool,
                "AI agent action expired"
            );
            return Err(anyhow!("agent action plan has expired"));
        }
        if !approved {
            info!(
                target: "focaldesk.ai",
                plan_id = %plan_id,
                tool = %pending.action.tool,
                "AI agent action denied by client"
            );
            return Ok(AgentActionResponse {
                plan_id,
                tool: pending.action.tool,
                executed: false,
                result: None,
            });
        }

        if let Err(err) = confirm_ai_action(
            &pending.action.tool,
            &format!("Approve AI action: {}?", pending.action.tool),
            &format!(
                "Plan ID: {plan_id}\nExact arguments: {}\nThis approval applies once and cannot be remembered.",
                pending.action.arguments
            ),
        ) {
            info!(
                target: "focaldesk.ai",
                plan_id = %plan_id,
                tool = %pending.action.tool,
                error = %err,
                "AI agent action not approved by native confirmation"
            );
            return Err(err);
        }

        let executor = self
            .tool_executor
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("AI agent tools are not configured"))?;
        let result = executor
            .execute_confirmed(&pending.action.tool, pending.action.arguments.clone())
            .await
            .with_context(|| format!("confirmed agent action {} failed", pending.action.tool))?;
        info!(
            target: "focaldesk.ai",
            plan_id = %plan_id,
            tool = %pending.action.tool,
            "AI agent action executed"
        );
        Ok(AgentActionResponse {
            plan_id,
            tool: pending.action.tool,
            executed: true,
            result: Some(result),
        })
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

/// Prefer the ACL-protected broker and preserve environment variables as a
/// development/upgrade fallback. Broker failures are expected on systems that
/// have not installed focald-secrets yet, so they are debug-level only.
fn credential(broker_key: &str, environment_key: &str) -> Option<Zeroizing<String>> {
    match focaldesk_secrets_client::get(broker_key) {
        Ok(value) => {
            debug!(
                target: "focaldesk.ai",
                key = broker_key,
                "loaded credential from focald-secrets"
            );
            Some(value)
        }
        Err(error) => {
            debug!(
                target: "focaldesk.ai",
                key = broker_key,
                %error,
                "credential unavailable from focald-secrets; checking environment"
            );
            std::env::var(environment_key).ok().map(Zeroizing::new)
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

    let policy = memory_policy_from_env()?;
    let store = MemoryStore::open_default_with_policy(dimension, policy)
        .context("failed to open default AI memory store")?;
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(
        OllamaEmbeddingProvider::new(ollama_base.to_string(), model.clone(), dimension)
            .context("failed to build Ollama embedding provider")?,
    );

    info!(
        target: "focaldesk.ai",
        model = %model,
        dimension,
        retention_days = ?policy.retention.map(|duration| duration.as_secs() / 86_400),
        max_entries = ?policy.max_entries,
        "AI memory store enabled"
    );

    Ok(MemoryService::new(store, embedder))
}

fn memory_policy_from_env() -> Result<MemoryPolicy> {
    let retention_days = parse_memory_limit("FOCALDESK_MEMORY_RETENTION_DAYS", 90, 36_500)?;
    let max_entries = parse_memory_limit("FOCALDESK_MEMORY_MAX_ENTRIES", 10_000, 1_000_000)?;
    Ok(MemoryPolicy {
        retention: retention_days.map(|days| Duration::from_secs(days as u64 * 86_400)),
        max_entries,
    })
}

fn parse_memory_limit(name: &str, default: usize, maximum: usize) -> Result<Option<usize>> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("{name} must be a non-negative integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("failed to read {name}")),
    };
    if value == 0 {
        return Ok(None);
    }
    if value > maximum {
        return Err(anyhow!(
            "{name} exceeds the maximum supported value {maximum}"
        ));
    }
    Ok(Some(value))
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

fn random_plan_id() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicUsize;

    struct ConfirmTrackingExecutor {
        confirmed_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AgentToolExecutor for ConfirmTrackingExecutor {
        fn tools(&self) -> Vec<crate::AgentToolSpec> {
            vec![crate::AgentToolSpec {
                name: "focus_window".into(),
                description: "Focus a window".into(),
                input_schema: json!({"type":"object"}),
                mutating: true,
            }]
        }

        async fn execute(&self, _tool: &str, _arguments: Value) -> Result<Value> {
            unreachable!("mutating tools must not use the read-only execution path")
        }

        async fn execute_confirmed(&self, _tool: &str, _arguments: Value) -> Result<Value> {
            self.confirmed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"focused":true}))
        }
    }

    fn service_with_pending_action(
        plan_id: &str,
        expires_at: std::time::Instant,
    ) -> (AiService, Arc<AtomicUsize>) {
        let confirmed_calls = Arc::new(AtomicUsize::new(0));
        let service =
            AiService::new("test").with_tool_executor(Arc::new(ConfirmTrackingExecutor {
                confirmed_calls: confirmed_calls.clone(),
            }));
        service.pending_agent_actions.lock().unwrap().insert(
            plan_id.into(),
            PendingAgentAction {
                action: AgentProposedAction {
                    tool: "focus_window".into(),
                    arguments: json!({"id":7}),
                },
                expires_at,
            },
        );
        (service, confirmed_calls)
    }

    #[test]
    fn activity_guard_reports_and_clears_in_flight_work() {
        let counter = AtomicUsize::new(0);
        {
            let _guard = ActivityGuard::new(&counter);
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

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

    #[test]
    fn agent_plan_ids_are_random_fixed_width_hex() {
        let first = random_plan_id();
        let second = random_plan_id();
        assert_eq!(first.len(), 48);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn denied_agent_action_is_consumed_without_execution_or_replay() {
        let plan_id = "a".repeat(48);
        let (service, confirmed_calls) = service_with_pending_action(
            &plan_id,
            std::time::Instant::now() + Duration::from_secs(60),
        );

        let response = service
            .confirm_agent_action(plan_id.clone(), false)
            .await
            .unwrap();
        assert!(!response.executed);
        assert!(response.result.is_none());
        assert_eq!(confirmed_calls.load(Ordering::SeqCst), 0);

        let replay = service
            .confirm_agent_action(plan_id, false)
            .await
            .unwrap_err();
        assert!(
            replay
                .to_string()
                .contains("unknown, expired, or already resolved")
        );
    }

    #[tokio::test]
    async fn expired_agent_action_fails_before_native_prompt_or_execution() {
        let plan_id = "b".repeat(48);
        let (service, confirmed_calls) = service_with_pending_action(
            &plan_id,
            std::time::Instant::now() - Duration::from_millis(1),
        );

        let error = service
            .confirm_agent_action(plan_id.clone(), true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("plan has expired"));
        assert_eq!(confirmed_calls.load(Ordering::SeqCst), 0);
        assert!(service.pending_agent_actions.lock().unwrap().is_empty());
    }
}
