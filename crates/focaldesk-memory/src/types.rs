use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Row id of a stored memory, also used as the rowid of its paired vector row.
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
