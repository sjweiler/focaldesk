use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Stable identifier shared by SQLite metadata and the active vector index.
pub type MemoryId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub text: String,
    pub metadata: Value,
    pub created_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub record: MemoryRecord,
    /// Lower is more similar (sqlite-vec returns L2 distance by default).
    pub distance: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryPolicy {
    pub retention: Option<Duration>,
    pub max_entries: Option<usize>,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            retention: Some(Duration::from_secs(90 * 24 * 60 * 60)),
            max_entries: Some(10_000),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStatus {
    pub schema_version: u32,
    pub entry_count: u64,
    pub retention_days: Option<u64>,
    pub max_entries: Option<usize>,
    pub oldest_created_at_unix: Option<i64>,
    pub newest_created_at_unix: Option<i64>,
    /// Vector backend serving semantic queries (for example `sqlite-vec` or
    /// `focal-vector`).
    #[serde(default)]
    pub vector_backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedDocument {
    pub source: String,
    pub title: String,
    pub media_type: String,
    pub content_hash: String,
    pub modified_at_unix: i64,
    pub indexed_at_unix: i64,
    pub chunk_count: usize,
    pub memory_ids: Vec<MemoryId>,
}
