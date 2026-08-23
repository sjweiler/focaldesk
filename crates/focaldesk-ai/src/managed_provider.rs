use anyhow::{Result, anyhow};
use async_trait::async_trait;
use rand::RngCore;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout_at};

use crate::provider::{AiProvider, ProviderError, ProviderErrorKind, provider_error};
use crate::types::{ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo, ProviderTelemetry};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub overall_timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(2),
            overall_timeout: Duration::from_secs(120),
        }
    }
}

pub(crate) struct ManagedProvider {
    inner: Arc<dyn AiProvider>,
    telemetry: Arc<Mutex<BTreeMap<String, ProviderTelemetry>>>,
    policy: RetryPolicy,
}

impl ManagedProvider {
    pub(crate) fn new(
        inner: Arc<dyn AiProvider>,
        telemetry: Arc<Mutex<BTreeMap<String, ProviderTelemetry>>>,
        policy: RetryPolicy,
    ) -> Self {
        Self {
            inner,
            telemetry,
            policy,
        }
    }

    async fn retry_delay(&self, attempt: u32, retry_after: Option<Duration>) {
        let shift = attempt.saturating_sub(1).min(16);
        let exponential_ms = self
            .policy
            .base_delay
            .as_millis()
            .saturating_mul(1_u128 << shift)
            .min(self.policy.max_delay.as_millis()) as u64;
        let jitter_limit = exponential_ms / 4;
        let mut random = [0_u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let jitter = if jitter_limit == 0 {
            0
        } else {
            u64::from_le_bytes(random) % (jitter_limit + 1)
        };
        let exponential =
            Duration::from_millis(exponential_ms.saturating_add(jitter)).min(self.policy.max_delay);
        sleep(
            retry_after
                .unwrap_or(exponential)
                .min(self.policy.max_delay),
        )
        .await;
    }
}

#[async_trait]
impl AiProvider for ManagedProvider {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        let info = self.info();
        let mut telemetry = TelemetryRequest::new(info.id, 0, self.telemetry.clone());
        let deadline = tokio::time::Instant::now() + self.policy.overall_timeout;
        for attempt in 1..=self.policy.max_attempts {
            match timeout_at(deadline, self.inner.list_models()).await {
                Ok(Ok(models)) => {
                    telemetry.success(0, None);
                    return Ok(models);
                }
                Ok(Err(error)) => {
                    let classification = provider_error(&error);
                    let retryable = classification.is_some_and(|error| error.kind.is_retryable());
                    if retryable && attempt < self.policy.max_attempts {
                        telemetry.retry();
                        let retry_after = classification.and_then(|error| error.retry_after);
                        if timeout_at(deadline, self.retry_delay(attempt, retry_after))
                            .await
                            .is_err()
                        {
                            telemetry.timeout();
                            return Err(ProviderError::new(
                                ProviderErrorKind::Timeout,
                                format!(
                                    "AI provider timed out after {} seconds",
                                    self.policy.overall_timeout.as_secs()
                                ),
                            )
                            .into());
                        }
                        continue;
                    }
                    telemetry.failure(&error);
                    return Err(error);
                }
                Err(_) => {
                    telemetry.timeout();
                    return Err(ProviderError::new(
                        ProviderErrorKind::Timeout,
                        format!(
                            "AI provider timed out after {} seconds",
                            self.policy.overall_timeout.as_secs()
                        ),
                    )
                    .into());
                }
            }
        }
        unreachable!("retry loop always returns")
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let info = self.info();
        let mut telemetry =
            TelemetryRequest::new(info.id, request_size(&request), self.telemetry.clone());
        let deadline = tokio::time::Instant::now() + self.policy.overall_timeout;

        for attempt in 1..=self.policy.max_attempts {
            let result = timeout_at(deadline, self.inner.chat(request.clone())).await;
            match result {
                Ok(Ok(response)) => {
                    telemetry.success(response.content.len(), response.usage);
                    return Ok(response);
                }
                Ok(Err(error)) => {
                    let classification = provider_error(&error);
                    let retryable = classification.is_some_and(|error| error.kind.is_retryable());
                    if retryable && attempt < self.policy.max_attempts {
                        telemetry.retry();
                        let retry_after = classification.and_then(|error| error.retry_after);
                        if timeout_at(deadline, self.retry_delay(attempt, retry_after))
                            .await
                            .is_err()
                        {
                            telemetry.timeout();
                            return Err(ProviderError::new(
                                ProviderErrorKind::Timeout,
                                format!(
                                    "AI provider timed out after {} seconds",
                                    self.policy.overall_timeout.as_secs()
                                ),
                            )
                            .into());
                        }
                        continue;
                    }
                    telemetry.failure(&error);
                    return Err(error);
                }
                Err(_) => {
                    telemetry.timeout();
                    return Err(ProviderError::new(
                        ProviderErrorKind::Timeout,
                        format!(
                            "AI provider timed out after {} seconds",
                            self.policy.overall_timeout.as_secs()
                        ),
                    )
                    .into());
                }
            }
        }
        unreachable!("retry loop always returns")
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        deltas: mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        let info = self.info();
        let mut telemetry =
            TelemetryRequest::new(info.id, request_size(&request), self.telemetry.clone());
        let deadline = tokio::time::Instant::now() + self.policy.overall_timeout;
        let mut emitted_delta = false;

        for attempt in 1..=self.policy.max_attempts {
            let (attempt_tx, mut attempt_rx) = mpsc::channel(32);
            let future = self.inner.chat_stream(request.clone(), attempt_tx);
            tokio::pin!(future);

            let result = loop {
                tokio::select! {
                    result = &mut future => break result,
                    delta = attempt_rx.recv() => {
                        if let Some(delta) = delta {
                            emitted_delta = true;
                            deltas.send(delta).await
                                .map_err(|_| anyhow!("AI stream consumer disconnected"))?;
                        }
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        telemetry.timeout();
                        return Err(ProviderError::new(
                            ProviderErrorKind::Timeout,
                            format!("AI provider timed out after {} seconds", self.policy.overall_timeout.as_secs()),
                        ).into());
                    }
                }
            };

            while let Ok(delta) = attempt_rx.try_recv() {
                emitted_delta = true;
                deltas
                    .send(delta)
                    .await
                    .map_err(|_| anyhow!("AI stream consumer disconnected"))?;
            }

            match result {
                Ok(response) => {
                    telemetry.success(response.content.len(), response.usage);
                    return Ok(response);
                }
                Err(error) => {
                    let classification = provider_error(&error);
                    let retryable = !emitted_delta
                        && classification.is_some_and(|error| error.kind.is_retryable());
                    if retryable && attempt < self.policy.max_attempts {
                        telemetry.retry();
                        let retry_after = classification.and_then(|error| error.retry_after);
                        if timeout_at(deadline, self.retry_delay(attempt, retry_after))
                            .await
                            .is_err()
                        {
                            telemetry.timeout();
                            return Err(ProviderError::new(
                                ProviderErrorKind::Timeout,
                                format!(
                                    "AI provider timed out after {} seconds",
                                    self.policy.overall_timeout.as_secs()
                                ),
                            )
                            .into());
                        }
                        continue;
                    }
                    telemetry.failure(&error);
                    return Err(error);
                }
            }
        }
        unreachable!("retry loop always returns")
    }
}

struct TelemetryRequest {
    provider: String,
    telemetry: Arc<Mutex<BTreeMap<String, ProviderTelemetry>>>,
    started: Instant,
    finished: bool,
}

impl TelemetryRequest {
    fn new(
        provider: String,
        input_bytes: usize,
        telemetry: Arc<Mutex<BTreeMap<String, ProviderTelemetry>>>,
    ) -> Self {
        update(&telemetry, &provider, |entry| {
            entry.requests = entry.requests.saturating_add(1);
            entry.input_bytes = entry.input_bytes.saturating_add(input_bytes as u64);
        });
        Self {
            provider,
            telemetry,
            started: Instant::now(),
            finished: false,
        }
    }

    fn retry(&mut self) {
        update(&self.telemetry, &self.provider, |entry| {
            entry.retries = entry.retries.saturating_add(1);
        });
    }

    fn success(&mut self, output_bytes: usize, usage: Option<crate::types::TokenUsage>) {
        self.finish(|entry, latency_ms| {
            entry.successes = entry.successes.saturating_add(1);
            entry.output_bytes = entry.output_bytes.saturating_add(output_bytes as u64);
            if let Some(usage) = usage {
                entry.input_tokens = entry.input_tokens.saturating_add(usage.input_tokens);
                entry.output_tokens = entry.output_tokens.saturating_add(usage.output_tokens);
            }
            entry.last_success_at_unix = Some(unix_now());
            entry.last_error = None;
            record_latency(entry, latency_ms);
        });
    }

    fn failure(&mut self, error: &anyhow::Error) {
        let message = provider_error(error)
            .map(|error| format!("{:?} provider failure", error.kind))
            .unwrap_or_else(|| "provider request failed".to_string());
        self.finish(move |entry, latency_ms| {
            entry.failures = entry.failures.saturating_add(1);
            entry.last_failure_at_unix = Some(unix_now());
            entry.last_error = Some(message);
            record_latency(entry, latency_ms);
        });
    }

    fn timeout(&mut self) {
        self.finish(|entry, latency_ms| {
            entry.failures = entry.failures.saturating_add(1);
            entry.timeouts = entry.timeouts.saturating_add(1);
            entry.last_failure_at_unix = Some(unix_now());
            entry.last_error = Some("provider request timed out".into());
            record_latency(entry, latency_ms);
        });
    }

    fn finish(&mut self, apply: impl FnOnce(&mut ProviderTelemetry, u64)) {
        let latency_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        update(&self.telemetry, &self.provider, |entry| {
            apply(entry, latency_ms)
        });
        self.finished = true;
    }
}

impl Drop for TelemetryRequest {
    fn drop(&mut self) {
        if !self.finished {
            let latency_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            update(&self.telemetry, &self.provider, |entry| {
                entry.cancellations = entry.cancellations.saturating_add(1);
                record_latency(entry, latency_ms);
            });
        }
    }
}

fn update(
    telemetry: &Mutex<BTreeMap<String, ProviderTelemetry>>,
    provider: &str,
    apply: impl FnOnce(&mut ProviderTelemetry),
) {
    let mut telemetry = telemetry
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let entry = telemetry
        .entry(provider.to_string())
        .or_insert_with(|| ProviderTelemetry {
            provider: provider.to_string(),
            ..ProviderTelemetry::default()
        });
    apply(entry);
}

fn record_latency(entry: &mut ProviderTelemetry, latency_ms: u64) {
    entry.last_latency_ms = Some(latency_ms);
    entry.total_latency_ms = entry.total_latency_ms.saturating_add(latency_ms);
}

fn request_size(request: &ChatRequest) -> usize {
    request.messages.iter().fold(0usize, |size, message| {
        size.saturating_add(message.content.len())
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SequenceProvider {
        attempts: AtomicUsize,
        failures_before_success: usize,
        kind: ProviderErrorKind,
    }

    struct DeltaThenFailProvider {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl AiProvider for DeltaThenFailProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "delta-failure".into(),
                kind: "test".into(),
                base_url: None,
                default_model: Some("test".into()),
            }
        }

        async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
            Ok(Vec::new())
        }

        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            unreachable!()
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
            deltas: mpsc::Sender<String>,
        ) -> Result<ChatResponse> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            deltas.send("partial".into()).await.unwrap();
            Err(ProviderError::new(ProviderErrorKind::Transient, "stream failed").into())
        }
    }

    struct PendingProvider;

    #[async_trait]
    impl AiProvider for PendingProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "pending".into(),
                kind: "test".into(),
                base_url: None,
                default_model: Some("test".into()),
            }
        }

        async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
            Ok(Vec::new())
        }

        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            std::future::pending().await
        }
    }

    #[async_trait]
    impl AiProvider for SequenceProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "sequence".into(),
                kind: "test".into(),
                base_url: None,
                default_model: Some("test".into()),
            }
        }

        async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
            Ok(Vec::new())
        }

        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.failures_before_success {
                return Err(ProviderError::new(self.kind, "planned failure").into());
            }
            Ok(ChatResponse {
                provider: "sequence".into(),
                model: Some("test".into()),
                content: "ok".into(),
                usage: Some(crate::types::TokenUsage {
                    input_tokens: 7,
                    output_tokens: 2,
                }),
            })
        }
    }

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            overall_timeout: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn transient_failures_retry_and_are_counted_once_per_request() {
        let inner = Arc::new(SequenceProvider {
            attempts: AtomicUsize::new(0),
            failures_before_success: 2,
            kind: ProviderErrorKind::Transient,
        });
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let managed = ManagedProvider::new(inner.clone(), telemetry.clone(), fast_policy());

        managed
            .chat(ChatRequest::from_prompt("hello"))
            .await
            .unwrap();

        assert_eq!(inner.attempts.load(Ordering::SeqCst), 3);
        let snapshot = telemetry.lock().unwrap()["sequence"].clone();
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.successes, 1);
        assert_eq!(snapshot.retries, 2);
        assert_eq!(snapshot.failures, 0);
        assert_eq!(snapshot.input_tokens, 7);
        assert_eq!(snapshot.output_tokens, 2);
    }

    #[tokio::test]
    async fn permanent_failures_are_never_retried() {
        let inner = Arc::new(SequenceProvider {
            attempts: AtomicUsize::new(0),
            failures_before_success: usize::MAX,
            kind: ProviderErrorKind::Authentication,
        });
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let managed = ManagedProvider::new(inner.clone(), telemetry.clone(), fast_policy());

        managed
            .chat(ChatRequest::from_prompt("hello"))
            .await
            .unwrap_err();

        assert_eq!(inner.attempts.load(Ordering::SeqCst), 1);
        let snapshot = telemetry.lock().unwrap()["sequence"].clone();
        assert_eq!(snapshot.failures, 1);
        assert_eq!(snapshot.retries, 0);
    }

    #[tokio::test]
    async fn streaming_failure_after_a_delta_is_never_retried() {
        let inner = Arc::new(DeltaThenFailProvider {
            attempts: AtomicUsize::new(0),
        });
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let managed = ManagedProvider::new(inner.clone(), telemetry.clone(), fast_policy());
        let (tx, mut rx) = mpsc::channel(2);

        managed
            .chat_stream(ChatRequest::from_prompt("hello"), tx)
            .await
            .unwrap_err();

        assert_eq!(rx.recv().await.as_deref(), Some("partial"));
        assert_eq!(inner.attempts.load(Ordering::SeqCst), 1);
        let snapshot = telemetry.lock().unwrap()["delta-failure"].clone();
        assert_eq!(snapshot.failures, 1);
        assert_eq!(snapshot.retries, 0);
    }

    #[tokio::test]
    async fn dropping_an_in_flight_request_counts_as_cancellation() {
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let managed = Arc::new(ManagedProvider::new(
            Arc::new(PendingProvider),
            telemetry.clone(),
            fast_policy(),
        ));
        let task = tokio::spawn({
            let managed = managed.clone();
            async move { managed.chat(ChatRequest::from_prompt("hello")).await }
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;

        let snapshot = telemetry.lock().unwrap()["pending"].clone();
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.cancellations, 1);
        assert_eq!(snapshot.failures, 0);
    }

    #[tokio::test]
    async fn provider_timeout_is_typed_and_recorded() {
        let telemetry = Arc::new(Mutex::new(BTreeMap::new()));
        let managed = ManagedProvider::new(
            Arc::new(PendingProvider),
            telemetry.clone(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
                overall_timeout: Duration::from_millis(10),
            },
        );

        let error = managed
            .chat(ChatRequest::from_prompt("hello"))
            .await
            .unwrap_err();
        assert_eq!(
            provider_error(&error).map(|error| error.kind),
            Some(ProviderErrorKind::Timeout)
        );
        let snapshot = telemetry.lock().unwrap()["pending"].clone();
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.failures, 1);
        assert_eq!(snapshot.timeouts, 1);
        assert_eq!(snapshot.cancellations, 0);
    }
}
