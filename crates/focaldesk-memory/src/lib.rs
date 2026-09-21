//! Long-term memory support for the FocalDesk AI layer.
//!
//! SQLite is authoritative for text, document metadata, full-text search, and
//! recoverable embedding bytes. Similarity search uses the private FocalVector
//! sidecar by default, with sqlite-vec retained as a fallback backend.
//! [`EmbeddingProvider`] is the pluggable "text -> vector" step;
//! [`OllamaEmbeddingProvider`] is the first implementation.

mod embedding;
mod store;
mod types;

pub use embedding::{EmbeddingProvider, OllamaEmbeddingProvider};
pub use store::MemoryStore;
pub use types::{IndexedDocument, MemoryId, MemoryPolicy, MemoryRecord, MemoryStatus, SearchHit};

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
        self.store
            .recall_hybrid(query.to_string(), embedding, top_k)
            .await
    }

    pub async fn replace_document(
        &self,
        document: IndexedDocument,
        chunks: Vec<String>,
    ) -> Result<IndexedDocument> {
        let previous = self.store.document(&document.source).await?;
        let mut ids = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.into_iter().enumerate() {
            let metadata = serde_json::json!({
                "kind": "document_chunk",
                "source_uri": document.source,
                "title": document.title,
                "media_type": document.media_type,
                "content_hash": document.content_hash,
                "chunk_index": index,
                "chunk_count": document.chunk_count,
            });
            match self.remember_text(chunk, metadata).await {
                Ok(id) => ids.push(id),
                Err(error) => {
                    for id in ids {
                        let _ = self.store.forget(id).await;
                    }
                    return Err(error);
                }
            }
        }
        let mut indexed = document;
        indexed.memory_ids = ids;
        if let Err(error) = self.store.save_document(indexed.clone()).await {
            for id in &indexed.memory_ids {
                let _ = self.store.forget(*id).await;
            }
            return Err(error);
        }
        if let Some(previous) = previous {
            for id in previous.memory_ids {
                if let Err(error) = self.store.forget(id).await {
                    tracing::warn!(
                        target: "focaldesk.memory",
                        memory_id = id,
                        %error,
                        "failed to retire a superseded document chunk"
                    );
                }
            }
        }
        Ok(indexed)
    }

    pub async fn documents(&self) -> Result<Vec<IndexedDocument>> {
        self.store.documents().await
    }

    pub async fn remove_document(&self, source: &str) -> Result<bool> {
        let Some(document) = self.store.document(source).await? else {
            return Ok(false);
        };
        for id in &document.memory_ids {
            self.store.forget(*id).await?;
        }
        self.store.delete_document(source.to_string()).await?;
        Ok(true)
    }

    pub async fn forget(&self, id: MemoryId) -> Result<()> {
        self.store.forget(id).await
    }

    pub async fn clear(&self) -> Result<usize> {
        self.store.clear().await
    }

    pub async fn status(&self) -> Result<MemoryStatus> {
        self.store.status().await
    }
}
