//! Long-term memory support for the FocalDesk AI layer.
//!
//! Storage is a local sqlite-vec-backed file (no server process): relational
//! metadata lives in a normal table, embeddings in a `vec0` virtual table,
//! joined by rowid. [`MemoryStore`] owns that file. [`EmbeddingProvider`] is
//! the pluggable "text -> vector" step; [`OllamaEmbeddingProvider`] is the
//! first implementation. [`MemoryService`] wires the two together so callers
//! can work with plain text instead of raw vectors.

mod embedding;
mod store;
mod types;

pub use embedding::{EmbeddingProvider, OllamaEmbeddingProvider};
pub use store::MemoryStore;
pub use types::{MemoryId, MemoryRecord, SearchHit};

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;

/// Convenience wrapper that embeds text on the way in and out, so callers
/// don't have to juggle `Vec<f32>` themselves.
#[derive(Clone)]
pub struct MemoryService {
    store: MemoryStore,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl MemoryService {
    pub fn new(store: MemoryStore, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { store, embedder }
    }

    pub async fn remember_text(
        &self,
        text: impl Into<String>,
        metadata: Value,
    ) -> Result<MemoryId> {
        let text = text.into();
        let embedding = self.embedder.embed(&text).await?;
        self.store.remember(text, embedding, metadata).await
    }

    pub async fn recall_similar(&self, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        let embedding = self.embedder.embed(query).await?;
        self.store.recall(embedding, top_k).await
    }

    pub async fn forget(&self, id: MemoryId) -> Result<()> {
        self.store.forget(id).await
    }
}
