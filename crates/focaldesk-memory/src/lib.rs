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
        // Complete and validate the whole batch before mutating storage. An
        // Ollama or dimension failure therefore leaves the prior document
        // fully intact and cannot create partial replacement chunks.
        let embeddings = self.embedder.embed_batch(&chunks).await?;
        if embeddings.len() != chunks.len() {
            anyhow::bail!(
                "embedding provider returned {} embeddings for {} document chunks",
                embeddings.len(),
                chunks.len()
            );
        }
        let mut ids = Vec::with_capacity(chunks.len());
        for (index, (chunk, embedding)) in chunks.into_iter().zip(embeddings).enumerate() {
            let metadata = serde_json::json!({
                "kind": "document_chunk",
                "source_uri": document.source,
                "title": document.title,
                "media_type": document.media_type,
                "content_hash": document.content_hash,
                "modified_at_unix": document.modified_at_unix,
                "indexed_at_unix": document.indexed_at_unix,
                "chunk_index": index,
                "chunk_count": document.chunk_count,
            });
            match self.store.remember(chunk, embedding, metadata).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct BatchEmbedder {
        batch_calls: AtomicUsize,
        fail: AtomicBool,
        invalid_second_dimension: AtomicBool,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for BatchEmbedder {
        fn dimension(&self) -> usize {
            3
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            panic!("document replacement should use the batch API")
        }

        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.batch_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                bail!("batch embedding failed");
            }
            let mut embeddings = texts
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let mut embedding = vec![0.0; 3];
                    embedding[index % 3] = 1.0;
                    embedding
                })
                .collect::<Vec<_>>();
            if self.invalid_second_dimension.load(Ordering::SeqCst) && embeddings.len() > 1 {
                embeddings[1].pop();
            }
            Ok(embeddings)
        }
    }

    fn test_path(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "focaldesk-memory-{name}-{}-{stamp}.db",
            std::process::id()
        ))
    }

    fn document(hash: &str) -> IndexedDocument {
        IndexedDocument {
            source: "/tmp/batched-document.md".into(),
            title: "batched-document.md".into(),
            media_type: "text/markdown".into(),
            content_hash: hash.into(),
            modified_at_unix: 1,
            indexed_at_unix: 2,
            chunk_count: 2,
            memory_ids: Vec::new(),
        }
    }

    #[tokio::test]
    async fn document_replacement_embeds_all_chunks_in_one_batch() {
        let path = test_path("batch");
        let store = MemoryStore::open(&path, 3).unwrap();
        let embedder = Arc::new(BatchEmbedder {
            batch_calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            invalid_second_dimension: AtomicBool::new(false),
        });
        let memory = MemoryService::new(store, embedder.clone());

        let indexed = memory
            .replace_document(document("new"), vec!["one".into(), "two".into()])
            .await
            .unwrap();

        assert_eq!(embedder.batch_calls.load(Ordering::SeqCst), 1);
        assert_eq!(indexed.memory_ids.len(), 2);
        assert_eq!(memory.status().await.unwrap().entry_count, 2);
        drop(memory);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn failed_batch_preserves_previous_document_and_chunks() {
        let path = test_path("batch-rollback");
        let store = MemoryStore::open(&path, 3).unwrap();
        let embedder = Arc::new(BatchEmbedder {
            batch_calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            invalid_second_dimension: AtomicBool::new(false),
        });
        let memory = MemoryService::new(store, embedder.clone());
        let previous = memory
            .replace_document(document("old"), vec!["old one".into(), "old two".into()])
            .await
            .unwrap();
        embedder.fail.store(true, Ordering::SeqCst);

        let error = memory
            .replace_document(document("new"), vec!["new one".into(), "new two".into()])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("batch embedding failed"));
        assert_eq!(memory.documents().await.unwrap(), vec![previous]);
        assert_eq!(memory.status().await.unwrap().entry_count, 2);
        drop(memory);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn partial_storage_failure_rolls_back_new_chunks() {
        let path = test_path("storage-rollback");
        let store = MemoryStore::open(&path, 3).unwrap();
        let embedder = Arc::new(BatchEmbedder {
            batch_calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            invalid_second_dimension: AtomicBool::new(false),
        });
        let memory = MemoryService::new(store, embedder.clone());
        let previous = memory
            .replace_document(document("old"), vec!["old one".into(), "old two".into()])
            .await
            .unwrap();
        embedder
            .invalid_second_dimension
            .store(true, Ordering::SeqCst);

        let error = memory
            .replace_document(document("new"), vec!["new one".into(), "new two".into()])
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("embedding has 2 dims, store expects 3"));
        assert_eq!(memory.documents().await.unwrap(), vec![previous]);
        assert_eq!(memory.status().await.unwrap().entry_count, 2);
        drop(memory);
        let _ = std::fs::remove_file(path);
    }
}
