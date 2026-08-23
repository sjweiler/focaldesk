use anyhow::Result;
use async_trait::async_trait;
use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::types::{ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Transient,
    RateLimited,
    Timeout,
    Authentication,
    InvalidRequest,
    Protocol,
    Permanent,
}

impl ProviderErrorKind {
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient | Self::RateLimited | Self::Timeout)
    }
}

#[derive(Debug)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub retry_after: Option<Duration>,
    message: String,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            retry_after: None,
            message: message.into(),
        }
    }

    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }

    pub fn from_http(provider: &str, status: reqwest::StatusCode, body: &str) -> Self {
        let kind = match status.as_u16() {
            408 | 425 => ProviderErrorKind::Transient,
            429 => ProviderErrorKind::RateLimited,
            401 | 403 => ProviderErrorKind::Authentication,
            400..=499 => ProviderErrorKind::InvalidRequest,
            500..=599 => ProviderErrorKind::Transient,
            _ => ProviderErrorKind::Permanent,
        };
        let body = body.chars().take(1024).collect::<String>();
        Self::new(kind, format!("{provider} returned HTTP {status}: {body}"))
    }

    pub fn from_transport(context: &str, error: reqwest::Error) -> Self {
        let kind = if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else if error.is_connect() || error.is_request() || error.is_body() {
            ProviderErrorKind::Transient
        } else {
            ProviderErrorKind::Protocol
        };
        Self::new(kind, format!("{context}: {error}"))
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

pub fn provider_error(error: &anyhow::Error) -> Option<&ProviderError> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>>;

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Stream response text when supported. Providers without a native
    /// streaming transport retain compatibility by emitting one complete
    /// delta after their ordinary chat request finishes.
    async fn chat_stream(
        &self,
        request: ChatRequest,
        deltas: mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        let response = self.chat(request).await?;
        let _ = deltas.send(response.content.clone()).await;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_retry_classification_is_fail_closed() {
        assert_eq!(
            ProviderError::from_http("test", reqwest::StatusCode::TOO_MANY_REQUESTS, "").kind,
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            ProviderError::from_http("test", reqwest::StatusCode::BAD_GATEWAY, "").kind,
            ProviderErrorKind::Transient
        );
        assert!(
            !ProviderError::from_http("test", reqwest::StatusCode::UNAUTHORIZED, "")
                .kind
                .is_retryable()
        );
        assert!(
            !ProviderError::from_http("test", reqwest::StatusCode::BAD_REQUEST, "")
                .kind
                .is_retryable()
        );
    }
}
