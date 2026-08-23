use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::{bail, Context, Result};
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, Transaction};
use serde_json::Value;
use zerocopy::IntoBytes;

use crate::types::{MemoryId, MemoryPolicy, MemoryRecord, MemoryStatus, SearchHit};

const MEMORY_SCHEMA_VERSION: u32 = 2;

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
    policy: MemoryPolicy,
}

impl MemoryStore {
    /// Opens (creating if needed) the memory store at `path`, sized for
    /// vectors of `dimension` floats. `vec0` fixes the column width at
    /// table-creation time, so reopening an existing file with a different
    /// dimension is an error rather than silently truncating/padding.
    pub fn open(path: impl AsRef<Path>, dimension: usize) -> Result<Self> {
        Self::open_with_policy(path, dimension, MemoryPolicy::default())
    }

    pub fn open_with_policy(
        path: impl AsRef<Path>,
        dimension: usize,
        policy: MemoryPolicy,
    ) -> Result<Self> {
        register_sqlite_vec();

        if policy.max_entries == Some(0) {
            bail!("memory max_entries must be greater than zero or disabled");
        }

        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            if parent.file_name().is_some_and(|name| name == "focaldesk") {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("failed to protect {}", parent.display()))?;
            }
        }

        let mut conn = Connection::open(path.as_ref())
            .with_context(|| format!("failed to open {}", path.as_ref().display()))?;
        fs::set_permissions(path.as_ref(), fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", path.as_ref().display()))?;

        Self::init_schema(&mut conn, dimension, policy)?;
        Self::prune_locked(&mut conn, policy, now_unix())?;

        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            dimension,
            policy,
        })
    }

    /// Opens the store at the default per-user data location
    /// (`$XDG_DATA_HOME/focaldesk/memory.db`).
    pub fn open_default(dimension: usize) -> Result<Self> {
        Self::open_default_with_policy(dimension, MemoryPolicy::default())
    }

    pub fn open_default_with_policy(dimension: usize, policy: MemoryPolicy) -> Result<Self> {
        let path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("focaldesk")
            .join("memory.db");
        Self::open_with_policy(path, dimension, policy)
    }

    fn init_schema(conn: &mut Connection, dimension: usize, policy: MemoryPolicy) -> Result<()> {
        let current_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if current_version > MEMORY_SCHEMA_VERSION {
            bail!(
                "memory store schema version {current_version} is newer than supported version {MEMORY_SCHEMA_VERSION}"
            );
        }

        let tx = conn.transaction()?;
        tx.execute_batch(
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

        let has_expiry: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('memories') WHERE name = 'expires_at_unix')",
            [],
            |row| row.get(0),
        )?;
        if !has_expiry {
            tx.execute(
                "ALTER TABLE memories ADD COLUMN expires_at_unix INTEGER",
                [],
            )?;
        }

        let existing_dim: Option<String> = tx
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
                tx.execute(
                    "INSERT INTO memory_meta (key, value) VALUES ('dimension', ?1)",
                    params![dimension.to_string()],
                )?;
            }
        }

        tx.execute(
            &format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(embedding float[{dimension}])"
            ),
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS memories_expires_at_idx ON memories(expires_at_unix)",
            [],
        )?;

        if let Some(retention) = policy.retention {
            let retention_seconds = retention.as_secs().min(i64::MAX as u64) as i64;
            tx.execute(
                "UPDATE memories SET expires_at_unix = created_at_unix + ?1",
                params![retention_seconds],
            )?;
        } else {
            tx.execute("UPDATE memories SET expires_at_unix = NULL", [])?;
        }

        if current_version < 2 {
            tx.pragma_update(None, "user_version", MEMORY_SCHEMA_VERSION)?;
        }

        tx.commit()?;

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
        let policy = self.policy;
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            let created_at_unix = now_unix();
            let expires_at_unix = policy.retention.map(|retention| {
                created_at_unix.saturating_add(retention.as_secs().min(i64::MAX as u64) as i64)
            });

            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO memories (text, metadata, created_at_unix, expires_at_unix)
                 VALUES (?1, ?2, ?3, ?4)",
                params![text, metadata.to_string(), created_at_unix, expires_at_unix],
            )?;
            let id = tx.last_insert_rowid();

            tx.execute(
                "INSERT INTO memory_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![id, embedding.as_bytes()],
            )?;
            tx.commit()?;
            Self::prune_locked(&mut conn, policy, created_at_unix)?;

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
        let policy = self.policy;
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::prune_locked(&mut conn, policy, now_unix())?;

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
            let mut conn = inner.lock().expect("memory store connection poisoned");
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM memory_vectors WHERE rowid = ?1", params![id])?;
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn clear(&self) -> Result<usize> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            let tx = conn.transaction()?;
            let deleted: usize =
                tx.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
            tx.execute("DELETE FROM memory_vectors", [])?;
            tx.execute("DELETE FROM memories", [])?;
            tx.commit()?;
            Ok(deleted)
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn status(&self) -> Result<MemoryStatus> {
        let inner = self.inner.clone();
        let policy = self.policy;
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::prune_locked(&mut conn, policy, now_unix())?;
            let (entry_count, oldest_created_at_unix, newest_created_at_unix) = conn.query_row(
                "SELECT COUNT(*), MIN(created_at_unix), MAX(created_at_unix) FROM memories",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            Ok(MemoryStatus {
                schema_version: MEMORY_SCHEMA_VERSION,
                entry_count,
                retention_days: policy.retention.map(|duration| duration.as_secs() / 86_400),
                max_entries: policy.max_entries,
                oldest_created_at_unix,
                newest_created_at_unix,
            })
        })
        .await
        .context("memory store task panicked")?
    }

    fn prune_locked(conn: &mut Connection, policy: MemoryPolicy, now: i64) -> Result<usize> {
        let tx = conn.transaction()?;
        let mut deleted = 0usize;

        let expired_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM memories
                 WHERE expires_at_unix IS NOT NULL AND expires_at_unix <= ?1",
            )?;
            let ids = stmt
                .query_map(params![now], |row| row.get::<_, MemoryId>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        deleted = deleted.saturating_add(Self::delete_ids(&tx, &expired_ids)?);

        if let Some(max_entries) = policy.max_entries {
            let count: usize =
                tx.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
            let excess = count.saturating_sub(max_entries);
            if excess > 0 {
                let oldest_ids = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM memories ORDER BY created_at_unix ASC, id ASC LIMIT ?1",
                    )?;
                    let ids = stmt
                        .query_map(params![excess as i64], |row| row.get::<_, MemoryId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    ids
                };
                deleted = deleted.saturating_add(Self::delete_ids(&tx, &oldest_ids)?);
            }
        }

        tx.commit()?;
        Ok(deleted)
    }

    fn delete_ids(tx: &Transaction<'_>, ids: &[MemoryId]) -> Result<usize> {
        for id in ids {
            tx.execute("DELETE FROM memory_vectors WHERE rowid = ?1", params![id])?;
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        }
        Ok(ids.len())
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

    fn test_path(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "focaldesk-memory-{label}-{}-{stamp}.db",
            std::process::id()
        ))
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build test runtime")
    }

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

    #[test]
    fn forgotten_memory_is_not_recalled() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "focaldesk-memory-forget-{}-{stamp}.db",
            std::process::id()
        ));
        let store = MemoryStore::open(&path, 4).expect("open memory store");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build test runtime");

        runtime.block_on(async {
            let vector = vec![1.0, 0.0, 0.0, 0.0];
            let id = store
                .remember("temporary fact".into(), vector.clone(), Value::Null)
                .await
                .expect("remember fact");
            assert_eq!(store.recall(vector.clone(), 1).await.unwrap().len(), 1);

            store.forget(id).await.expect("forget fact");
            assert!(store.recall(vector, 1).await.unwrap().is_empty());
        });

        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_schema_migrates_transactionally_to_v2() {
        let path = test_path("migration");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO memory_meta VALUES ('dimension', '4');
                 CREATE TABLE memories (
                    id INTEGER PRIMARY KEY,
                    text TEXT NOT NULL,
                    metadata TEXT NOT NULL DEFAULT '{}',
                    created_at_unix INTEGER NOT NULL
                 );
                 INSERT INTO memories (text, metadata, created_at_unix)
                 VALUES ('legacy memory', '{}', 2000000000);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        let store = MemoryStore::open_with_policy(
            &path,
            4,
            MemoryPolicy {
                retention: Some(std::time::Duration::from_secs(86_400)),
                max_entries: Some(100),
            },
        )
        .unwrap();
        let status = test_runtime().block_on(store.status()).unwrap();
        assert_eq!(status.schema_version, 2);
        assert_eq!(status.entry_count, 1);
        drop(store);

        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let expiry: Option<i64> = conn
            .query_row(
                "SELECT expires_at_unix FROM memories WHERE text = 'legacy memory'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 2);
        assert_eq!(expiry, Some(2_000_086_400));
        drop(conn);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn newer_schema_is_rejected_without_modification() {
        let path = test_path("newer-schema");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sentinel (value TEXT NOT NULL);
                 INSERT INTO sentinel VALUES ('untouched');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }

        let error = MemoryStore::open(&path, 4)
            .err()
            .expect("newer schema must fail closed");
        assert!(error.to_string().contains("newer than supported version 2"));

        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let sentinel: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        let memories_table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(sentinel, "untouched");
        assert!(!memories_table_exists);
        drop(conn);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn active_retention_policy_is_reapplied_when_reopened() {
        let path = test_path("retention-reopen");
        let store = MemoryStore::open_with_policy(
            &path,
            4,
            MemoryPolicy {
                retention: None,
                max_entries: None,
            },
        )
        .unwrap();
        test_runtime().block_on(async {
            store
                .remember(
                    "previously unbounded".into(),
                    vec![1.0, 0.0, 0.0, 0.0],
                    Value::Null,
                )
                .await
                .unwrap();
            assert_eq!(store.status().await.unwrap().entry_count, 1);
        });
        drop(store);

        let reopened = MemoryStore::open_with_policy(
            &path,
            4,
            MemoryPolicy {
                retention: Some(std::time::Duration::ZERO),
                max_entries: None,
            },
        )
        .unwrap();
        assert_eq!(
            test_runtime()
                .block_on(reopened.status())
                .unwrap()
                .entry_count,
            0
        );
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn retention_and_capacity_are_enforced_automatically() {
        let capacity_path = test_path("capacity");
        let capacity_store = MemoryStore::open_with_policy(
            &capacity_path,
            4,
            MemoryPolicy {
                retention: None,
                max_entries: Some(2),
            },
        )
        .unwrap();
        let runtime = test_runtime();
        runtime.block_on(async {
            let vector = vec![1.0, 0.0, 0.0, 0.0];
            for text in ["oldest", "middle", "newest"] {
                capacity_store
                    .remember(text.into(), vector.clone(), Value::Null)
                    .await
                    .unwrap();
            }
            assert_eq!(capacity_store.status().await.unwrap().entry_count, 2);
            let texts = capacity_store
                .recall(vector.clone(), 3)
                .await
                .unwrap()
                .into_iter()
                .map(|hit| hit.record.text)
                .collect::<Vec<_>>();
            assert!(!texts.iter().any(|text| text == "oldest"));

            let expiry_path = test_path("expiry");
            let expiry_store = MemoryStore::open_with_policy(
                &expiry_path,
                4,
                MemoryPolicy {
                    retention: Some(std::time::Duration::ZERO),
                    max_entries: None,
                },
            )
            .unwrap();
            expiry_store
                .remember("ephemeral".into(), vector, Value::Null)
                .await
                .unwrap();
            assert_eq!(expiry_store.status().await.unwrap().entry_count, 0);
            drop(expiry_store);
            let _ = fs::remove_file(expiry_path);
        });
        drop(capacity_store);
        let _ = fs::remove_file(capacity_path);
    }

    #[test]
    fn clear_is_atomic_and_reports_deleted_count() {
        let path = test_path("clear");
        let store = MemoryStore::open_with_policy(
            &path,
            4,
            MemoryPolicy {
                retention: None,
                max_entries: None,
            },
        )
        .unwrap();
        test_runtime().block_on(async {
            let vector = vec![1.0, 0.0, 0.0, 0.0];
            store
                .remember("one".into(), vector.clone(), Value::Null)
                .await
                .unwrap();
            store
                .remember("two".into(), vector, Value::Null)
                .await
                .unwrap();
            assert_eq!(store.clear().await.unwrap(), 2);
            assert_eq!(store.status().await.unwrap().entry_count, 0);
        });
        drop(store);
        let _ = fs::remove_file(path);
    }
}
