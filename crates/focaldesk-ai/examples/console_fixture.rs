//! Explicitly isolated backend for native console UI tests. Never used by the app.
use anyhow::{Context, Result};
use async_trait::async_trait;
use focaldesk_ai::provider::AiProvider;
use focaldesk_ai::{
    AiService, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo, TokenUsage,
};
use focaldesk_memory::{EmbeddingProvider, MemoryService, MemoryStore};
use std::sync::Arc;
struct Embeddings;
#[async_trait]
impl EmbeddingProvider for Embeddings {
    fn dimension(&self) -> usize {
        3
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Ok(vec![1., 0., 0.])
    }
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(texts.iter().map(|_| vec![1., 0., 0.]).collect())
    }
}
struct Provider;
#[async_trait]
impl AiProvider for Provider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            kind: "fixture".into(),
            base_url: None,
            default_model: Some("fixture-model".into()),
        }
    }
    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        Ok(vec![ProviderModelInfo {
            id: "fixture-model".into(),
        }])
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse {
            provider: "fixture".into(),
            model: Some("fixture-model".into()),
            content: format!(
                "## Fixture response\n\nTemperature: {}\n\nProject sources: {}\n\nImages: {}\n",
                request.temperature.unwrap_or(0.),
                request
                    .memory_sources
                    .as_ref()
                    .map(|s| s.len().to_string())
                    .unwrap_or("shared".into()),
                request
                    .messages
                    .iter()
                    .map(|m| m.images.len())
                    .sum::<usize>()
            ),
            usage: Some(TokenUsage {
                input_tokens: 20,
                output_tokens: 30,
            }),
            citations: vec![],
        })
    }
    async fn chat_stream(
        &self,
        request: ChatRequest,
        deltas: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        let long = request
            .messages
            .last()
            .is_some_and(|m| m.content.contains("long fixture"));
        let response = self.chat(request).await?;
        if long {
            for index in 0..100 {
                deltas
                    .send(format!("Fixture streaming line {index}.\n"))
                    .await?;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        } else {
            for line in response.content.split_inclusive('\n') {
                deltas.send(line.into()).await?;
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        }
        Ok(response)
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let root = std::path::PathBuf::from(
        std::env::var_os("FOCALDESK_CONSOLE_FIXTURE_ROOT")
            .context("Set an isolated fixture root")?,
    );
    if !root.starts_with(std::env::temp_dir()) {
        anyhow::bail!("Fixture storage must be inside the temporary directory");
    }
    std::fs::create_dir_all(&root)?;
    let memory = MemoryService::new(
        MemoryStore::open(root.join("memory.db"), 3)?,
        Arc::new(Embeddings),
    );
    let live_model = std::env::var("FOCALDESK_CONSOLE_LIVE_MODEL").ok();
    let mut service = AiService::new(if live_model.is_some() {
        "ollama"
    } else {
        "fixture"
    })
    .with_memory(memory);
    if let Some(model) = live_model {
        service.register(Arc::new(focaldesk_ai::providers::OllamaProvider::new(
            "http://127.0.0.1:11434",
            Some(model),
        )?));
    } else {
        service.register(Arc::new(Provider));
    }
    let socket = root.join("ai.sock");
    println!("Isolated UI fixture listening on {}", socket.display());
    focaldesk_ai::ipc::serve_ai_ipc_at(Arc::new(service), socket).await
}
