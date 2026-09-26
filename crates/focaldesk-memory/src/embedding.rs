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

    /// Embeds several texts at once. Providers with a native batch API should
    /// override this; the default preserves compatibility with providers that
    /// only implement single-text embedding.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut embeddings = Vec::with_capacity(texts.len());
        for text in texts {
            embeddings.push(self.embed(text).await?);
        }
        Ok(embeddings)
    }
}

/// Calls a local Ollama instance's batch-capable `/api/embed` endpoint. Reuses the
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
        let texts = [text.to_string()];
        let mut embeddings = self.embed_batch(&texts).await?;
        Ok(embeddings.remove(0))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&OllamaEmbedRequest {
                model: &self.model,
                input: texts,
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

        if decoded.embeddings.len() != texts.len() {
            bail!(
                "Ollama model '{}' returned {} embeddings for {} inputs",
                self.model,
                decoded.embeddings.len(),
                texts.len()
            );
        }

        decoded
            .embeddings
            .into_iter()
            .enumerate()
            .map(|(index, embedding)| {
                if embedding.len() != self.dimension {
                    bail!(
                        "Ollama model '{}' returned a {}-dim embedding for input {}, expected {}",
                        self.model,
                        embedding.len(),
                        index,
                        self.dimension
                    );
                }
                Ok(embedding.into_iter().map(|value| value as f32).collect())
            })
            .collect()
    }
}

#[derive(Debug, Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f64>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn serve_once(body: &'static str) -> (String, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() - header_end < content_length {
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
            }
            request_tx.send(request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        (format!("http://{address}"), request_rx)
    }

    #[tokio::test]
    async fn ollama_batches_inputs_in_one_request() {
        let (base_url, request) = serve_once(r#"{"embeddings":[[1,0,0],[0,1,0]]}"#);
        let provider = OllamaEmbeddingProvider::new(base_url, "test-model", 3).unwrap();

        let embeddings = provider
            .embed_batch(&["first".into(), "second".into()])
            .await
            .unwrap();

        assert_eq!(embeddings, vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]]);
        let request = request.recv().unwrap();
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let request_line = std::str::from_utf8(&request)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        assert_eq!(request_line, "POST /api/embed HTTP/1.1");
        let payload: serde_json::Value = serde_json::from_slice(&request[header_end..]).unwrap();
        assert_eq!(payload["model"], "test-model");
        assert_eq!(payload["input"], serde_json::json!(["first", "second"]));
    }

    #[tokio::test]
    async fn ollama_rejects_any_wrong_dimension_in_batch() {
        let (base_url, _request) = serve_once(r#"{"embeddings":[[1,0,0],[0,1]]}"#);
        let provider = OllamaEmbeddingProvider::new(base_url, "test-model", 3).unwrap();

        let error = provider
            .embed_batch(&["first".into(), "second".into()])
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("2-dim embedding for input 1, expected 3"));
    }
}
