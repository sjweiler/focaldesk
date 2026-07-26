use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::{bail, Context, Result};
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection};
use serde_json::Value;
use zerocopy::IntoBytes;

use crate::types::{MemoryId, MemoryRecord, SearchHit};

static EXTENSION_REGISTERED: std::sync::Once = std::sync::Once::new();

fn register_sqlite_vec() {
    EXTENSION_REGISTERED.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// A local, file-backed memory store: relational metadata in a normal SQLite
/// table, embeddings in a sqlite-vec `vec0` virtual table, joined by rowid.
/// No server process — this is just a `.db` file on disk.
#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<Mutex<Connection>>,
    dimension: usize,
}

impl MemoryStore {
    /// Opens (creating if needed) the memory store at `path`, sized for
    /// vectors of `dimension` floats. `vec0` fixes the column width at
    /// table-creation time, so reopening an existing file with a different
    /// dimension is an error rather than silently truncating/padding.
    pub fn open(path: impl AsRef<Path>, dimension: usize) -> Result<Self> {
        register_sqlite_vec();

        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            if parent.file_name().is_some_and(|name| name == "focaldesk") {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("failed to protect {}", parent.display()))?;
            }
        }

        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("failed to open {}", path.as_ref().display()))?;
        fs::set_permissions(path.as_ref(), fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", path.as_ref().display()))?;

        Self::init_schema(&conn, dimension)?;

        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            dimension,
        })
    }

    /// Opens the store at the default per-user data location
    /// (`$XDG_DATA_HOME/focaldesk/memory.db`).
    pub fn open_default(dimension: usize) -> Result<Self> {
        let path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("focaldesk")
            .join("memory.db");
        Self::open(path, dimension)
    }

    fn init_schema(conn: &Connection, dimension: usize) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY,
                text TEXT NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}',
                created_at_unix INTEGER NOT NULL
            );",
        )?;

        let existing_dim: Option<String> = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'dimension'",
                [],
                |row| row.get(0),
            )
            .ok();

        match existing_dim {
            Some(existing) if existing != dimension.to_string() => {
                bail!(
                    "memory store was created with dimension {existing}, but opened with {dimension}"
                );
            }
            Some(_) => {}
            None => {
                conn.execute(
                    "INSERT INTO memory_meta (key, value) VALUES ('dimension', ?1)",
                    params![dimension.to_string()],
                )?;
                conn.execute(
                    &format!(
                        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(embedding float[{dimension}])"
                    ),
                    [],
                )?;
            }
        }

        Ok(())
    }

    /// Stores `text` with its precomputed `embedding` and returns its id.
    /// The blocking sqlite work runs on a blocking thread so callers on the
    /// async runtime don't stall behind disk I/O.
    pub async fn remember(
        &self,
        text: String,
        embedding: Vec<f32>,
        metadata: Value,
    ) -> Result<MemoryId> {
        if embedding.len() != self.dimension {
            bail!(
                "embedding has {} dims, store expects {}",
                embedding.len(),
                self.dimension
            );
        }

        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");
            let created_at_unix = now_unix();

            conn.execute(
                "INSERT INTO memories (text, metadata, created_at_unix) VALUES (?1, ?2, ?3)",
                params![text, metadata.to_string(), created_at_unix],
            )?;
            let id = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO memory_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![id, embedding.as_bytes()],
            )?;

            Ok(id)
        })
        .await
        .context("memory store task panicked")?
    }

    /// Finds the `top_k` memories whose stored embedding is nearest to
    /// `query_embedding` (nearest first).
    pub async fn recall(&self, query_embedding: Vec<f32>, top_k: usize) -> Result<Vec<SearchHit>> {
        if query_embedding.len() != self.dimension {
            bail!(
                "query embedding has {} dims, store expects {}",
                query_embedding.len(),
                self.dimension
            );
        }

        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");

            let mut stmt = conn.prepare(
                "SELECT m.id, m.text, m.metadata, m.created_at_unix, v.distance
                 FROM memory_vectors v
                 JOIN memories m ON m.id = v.rowid
                 WHERE v.embedding MATCH ?1 AND k = ?2
                 ORDER BY v.distance",
            )?;

            let rows =
                stmt.query_map(params![query_embedding.as_bytes(), top_k as i64], |row| {
                    let metadata_json: String = row.get(2)?;
                    Ok(SearchHit {
                        record: MemoryRecord {
                            id: row.get(0)?,
                            text: row.get(1)?,
                            metadata: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
                            created_at_unix: row.get(3)?,
                        },
                        distance: row.get(4)?,
                    })
                })?;

            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(anyhow::Error::from)
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn forget(&self, id: MemoryId) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");
            conn.execute("DELETE FROM memory_vectors WHERE rowid = ?1", params![id])?;
            conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            Ok(())
        })
        .await
        .context("memory store task panicked")?
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn memory_database_is_private() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "focaldesk-memory-permissions-{}-{stamp}.db",
            std::process::id()
        ));

        let store = MemoryStore::open(&path, 4).expect("open memory store");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(store);
        let _ = fs::remove_file(path);
    }
}
