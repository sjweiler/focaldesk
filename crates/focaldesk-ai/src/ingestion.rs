use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IngestionSource {
    Document {
        path: std::path::PathBuf,
    },
    Directory {
        path: std::path::PathBuf,
        #[serde(default)]
        recursive: bool,
    },
    Website {
        url: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionJob {
    pub id: String,
    #[serde(default)]
    pub created_at_unix_ms: u64,
    pub source: IngestionSource,
    pub state: String,
    pub progress: Value,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub cancel_requested: bool,
}
pub(crate) struct IngestionControl {
    pub cancel: AtomicBool,
    pub status: Mutex<IngestionJob>,
}
impl IngestionControl {
    pub fn new(id: String, source: IngestionSource) -> Self {
        Self {
            cancel: AtomicBool::new(false),
            status: Mutex::new(IngestionJob {
                id,
                created_at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
                source,
                state: "queued".into(),
                progress: json!({"indexed":0,"unchanged":0,"skipped":0,"failed":0,"chunks":0}),
                result: None,
                error: None,
                cancel_requested: false,
            }),
        }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    pub fn progress(&self, value: impl Serialize) {
        if let Ok(mut status) = self.status.lock() {
            status.progress = serde_json::to_value(value).unwrap_or(Value::Null);
        }
    }
    pub fn snapshot(&self) -> anyhow::Result<IngestionJob> {
        self.status
            .lock()
            .map(|s| s.clone())
            .map_err(|_| anyhow::anyhow!("Indexing status unavailable"))
    }
}
