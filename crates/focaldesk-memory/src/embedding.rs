use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Turns text into a fixed-length vector. Implementations must always return
/// vectors of `dimension()` length so they can be stored in a single
/// sqlite-vec table (its column width is fixed at table-creation time).
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn dimension(&self) -> usize;

    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Calls a local Ollama instance's `/api/embeddings` endpoint. Reuses the
/// same base-url/client shape as `focaldesk_ai::providers::OllamaProvider`.
#[derive(Debug, Clone)]
pub struct OllamaEmbeddingProvider {
    base_url: String,
    model: String,
    dimension: usize,
    client: Client,
}

impl OllamaEmbeddingProvider {
    /// `dimension` must match what `model` actually emits (e.g. 768 for
    /// `nomic-embed-text`, 384 for `all-minilm`) — sqlite-vec has no way to
    /// discover this at runtime, so it's the caller's responsibility.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        dimension: usize,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build Ollama embedding HTTP client")?;

        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            dimension,
            client,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let response = self
            .client
            .post(format!("{}/api/embeddings", self.base_url))
            .json(&OllamaEmbedRequest {
                model: &self.model,
                prompt: text,
            })
            .send()
            .await
            .context("Ollama embedding request failed")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Ollama embedding response body")?;

        if !status.is_success() {
            bail!("Ollama returned HTTP {} while embedding: {}", status, body);
        }

        let decoded: OllamaEmbedResponse =
            serde_json::from_str(&body).context("failed to parse Ollama embedding response")?;

        if decoded.embedding.len() != self.dimension {
            bail!(
                "Ollama model '{}' returned a {}-dim embedding, expected {}",
                self.model,
                decoded.embedding.len(),
                self.dimension
            );
        }

        Ok(decoded.embedding.into_iter().map(|v| v as f32).collect())
    }
}

#[derive(Debug, Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f64>,
}
