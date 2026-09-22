use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::{bail, Context, Result};
use focal_vector_client::{Client as FocalVectorClient, Metric, Point};
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, Transaction};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use zerocopy::IntoBytes;

use crate::types::{
    IndexedDocument, MemoryId, MemoryPolicy, MemoryRecord, MemoryStatus, SearchHit,
};

const MEMORY_SCHEMA_VERSION: u32 = 4;
const FOCAL_VECTOR_COLLECTION: &str = "focaldesk-memories-v1";

static EXTENSION_REGISTERED: std::sync::Once = std::sync::Once::new();

fn register_sqlite_vec() {
    type SqliteExtensionEntry = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *mut std::ffi::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::ffi::c_int;
    EXTENSION_REGISTERED.call_once(|| unsafe {
        sqlite3_auto_extension(Some(
            std::mem::transmute::<*const (), SqliteExtensionEntry>(
                sqlite_vec::sqlite3_vec_init as *const (),
            ),
        ));
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
    vector_backend: VectorBackend,
}

#[derive(Clone)]
enum VectorBackend {
    SqliteVec,
    FocalVector {
        client: FocalVectorClient,
        collection: String,
    },
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

        Self::init_schema(&mut conn, dimension, policy, true)?;
        Self::import_focal_embeddings_to_sqlite(&mut conn)?;
        Self::prune_locked(&mut conn, policy, now_unix(), true)?;

        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            dimension,
            policy,
            vector_backend: VectorBackend::SqliteVec,
        })
    }

    /// Opens the authoritative SQLite memory database while using the local
    /// Focal Vector sidecar for similarity search. Embedding bytes are retained
    /// in SQLite so a missing or rebuilt vector collection can be restored.
    pub fn open_focal_vector_with_policy(
        path: impl AsRef<Path>,
        dimension: usize,
        policy: MemoryPolicy,
        collection: Option<String>,
    ) -> Result<Self> {
        register_sqlite_vec();
        if policy.max_entries == Some(0) {
            bail!("memory max_entries must be greater than zero or disabled");
        }
        protect_parent(path.as_ref())?;

        let mut conn = Connection::open(path.as_ref())
            .with_context(|| format!("failed to open {}", path.as_ref().display()))?;
        fs::set_permissions(path.as_ref(), fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", path.as_ref().display()))?;
        Self::init_schema(&mut conn, dimension, policy, false)?;

        let client = FocalVectorClient::from_environment()
            .context("failed to configure Focal Vector client")?;
        wait_for_focal_vector(&client)?;
        let collection = collection.unwrap_or_else(|| FOCAL_VECTOR_COLLECTION.to_string());
        let collections = client
            .list_collections()
            .context("failed to list Focal Vector collections")?;
        let embedding_count: usize =
            conn.query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
                row.get(0)
            })?;
        match collections.iter().find(|item| item.name == collection) {
            Some(existing)
                if existing.dimension != dimension || existing.metric != Metric::Cosine =>
            {
                bail!(
                    "Focal Vector collection '{}' has dimension {} and metric {:?}; expected dimension {} and cosine",
                    collection,
                    existing.dimension,
                    existing.metric,
                    dimension
                );
            }
            Some(existing) if existing.points != embedding_count => {
                // The vector collection is a rebuildable index. A count
                // mismatch means a previous mutation was interrupted or the
                // collection was restored independently of SQLite.
                conn.execute("UPDATE memory_embeddings SET indexed = 0", [])?;
            }
            Some(_) => {}
            None => {
                client
                    .create_collection(collection.clone(), dimension, Metric::Cosine)
                    .context("failed to create Focal Vector memory collection")?;
                conn.execute("UPDATE memory_embeddings SET indexed = 0", [])?;
            }
        }

        Self::import_legacy_embeddings(&mut conn)?;
        let backend = VectorBackend::FocalVector { client, collection };
        Self::sync_pending_embeddings(&mut conn, &backend)?;
        Self::prune_backend_locked(&mut conn, policy, now_unix(), &backend)?;

        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            dimension,
            policy,
            vector_backend: backend,
        })
    }

    pub fn open_default_focal_vector_with_policy(
        dimension: usize,
        policy: MemoryPolicy,
        collection: Option<String>,
    ) -> Result<Self> {
        let path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("focaldesk")
            .join("memory.db");
        Self::open_focal_vector_with_policy(path, dimension, policy, collection)
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

    fn init_schema(
        conn: &mut Connection,
        dimension: usize,
        policy: MemoryPolicy,
        create_sqlite_vec: bool,
    ) -> Result<()> {
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
            );
            CREATE TABLE IF NOT EXISTS memory_embeddings (
                memory_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL,
                indexed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS indexed_documents (
                source TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                media_type TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                modified_at_unix INTEGER NOT NULL,
                indexed_at_unix INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                memory_ids TEXT NOT NULL
            );",
        )?;

        // Older SQLite-backed sessions could leave recoverable embedding rows
        // behind when memory was cleared before Focal Vector became the active
        // backend. Those orphan rows can collide with SQLite's reused integer
        // ids and make every subsequent document ingest fail.
        tx.execute(
            "DELETE FROM memory_embeddings
             WHERE memory_id NOT IN (SELECT id FROM memories)",
            [],
        )?;

        tx.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                text,
                content='memories',
                content_rowid='id',
                tokenize='unicode61 remove_diacritics 2'
            );
            CREATE TRIGGER IF NOT EXISTS memories_fts_insert AFTER INSERT ON memories BEGIN
                INSERT INTO memory_fts(rowid, text) VALUES (new.id, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_fts_delete AFTER DELETE ON memories BEGIN
                INSERT INTO memory_fts(memory_fts, rowid, text) VALUES ('delete', old.id, old.text);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_fts_update AFTER UPDATE OF text ON memories BEGIN
                INSERT INTO memory_fts(memory_fts, rowid, text) VALUES ('delete', old.id, old.text);
                INSERT INTO memory_fts(rowid, text) VALUES (new.id, new.text);
            END;",
        )?;
        if current_version < 4 {
            tx.execute("INSERT INTO memory_fts(memory_fts) VALUES ('rebuild')", [])?;
        }

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

        if create_sqlite_vec {
            tx.execute(
                &format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(embedding float[{dimension}])"
                ),
                [],
            )?;
        }
        let has_legacy_vectors: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'memory_vectors'
            )",
            [],
            |row| row.get(0),
        )?;
        if has_legacy_vectors {
            tx.execute(
                "DELETE FROM memory_vectors
                 WHERE rowid NOT IN (SELECT id FROM memories)",
                [],
            )?;
        }
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

        if current_version < MEMORY_SCHEMA_VERSION {
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
        let backend = self.vector_backend.clone();
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

            match &backend {
                VectorBackend::SqliteVec => {
                    tx.execute(
                        "INSERT INTO memory_vectors (rowid, embedding) VALUES (?1, ?2)",
                        params![id, embedding.as_bytes()],
                    )?;
                }
                VectorBackend::FocalVector { .. } => {
                    tx.execute(
                        "INSERT INTO memory_embeddings (memory_id, embedding, indexed)
                         VALUES (?1, ?2, 0)",
                        params![id, embedding.as_bytes()],
                    )?;
                }
            }
            tx.commit()?;
            if matches!(backend, VectorBackend::FocalVector { .. }) {
                Self::sync_pending_embeddings(&mut conn, &backend)?;
            }
            Self::prune_backend_locked(&mut conn, policy, created_at_unix, &backend)?;

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
        let backend = self.vector_backend.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::prune_backend_locked(&mut conn, policy, now_unix(), &backend)?;

            match &backend {
                VectorBackend::SqliteVec => Self::recall_sqlite(&conn, query_embedding, top_k),
                VectorBackend::FocalVector { client, collection } => {
                    Self::sync_pending_embeddings(&mut conn, &backend)?;
                    let hits = client
                        .query(collection.clone(), query_embedding, top_k, None, None)
                        .context("Focal Vector memory query failed")?;
                    let mut recalled = Vec::with_capacity(hits.len());
                    for hit in hits {
                        let Ok(id) = hit.id.parse::<MemoryId>() else {
                            continue;
                        };
                        if let Some(record) = Self::record_by_id(&conn, id)? {
                            recalled.push(SearchHit {
                                record,
                                // Cosine scores are larger-is-better; retain the
                                // existing lower-is-better public distance API.
                                distance: 1.0 - hit.score,
                            });
                        }
                    }
                    Ok(recalled)
                }
            }
        })
        .await
        .context("memory store task panicked")?
    }

    /// Combines dense retrieval with SQLite FTS5 and applies a deterministic
    /// token-overlap reranker. Reciprocal-rank fusion keeps scores comparable
    /// without assuming either backend's raw score distribution.
    pub async fn recall_hybrid(
        &self,
        query: String,
        query_embedding: Vec<f32>,
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let candidate_count = top_k.saturating_mul(4).max(top_k);
        let dense = self.recall(query_embedding, candidate_count).await?;
        let inner = self.inner.clone();
        let query_for_lexical = query.clone();
        let lexical = tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");
            Self::recall_lexical(&conn, &query_for_lexical, candidate_count)
        })
        .await
        .context("memory lexical search task panicked")??;

        let query_terms = terms(&query);
        let mut fused: HashMap<MemoryId, (MemoryRecord, f32)> = HashMap::new();
        for (rank, hit) in dense.into_iter().enumerate() {
            let entry = fused
                .entry(hit.record.id)
                .or_insert_with(|| (hit.record, 0.0));
            entry.1 += 1.0 / (60.0 + rank as f32 + 1.0);
        }
        for (rank, hit) in lexical.into_iter().enumerate() {
            let entry = fused
                .entry(hit.record.id)
                .or_insert_with(|| (hit.record, 0.0));
            entry.1 += 1.0 / (60.0 + rank as f32 + 1.0);
        }
        let mut hits = fused
            .into_values()
            .map(|(record, mut score)| {
                let record_terms = terms(&record.text);
                if !query_terms.is_empty() {
                    let overlap = query_terms.intersection(&record_terms).count() as f32
                        / query_terms.len() as f32;
                    score += overlap * 0.02;
                }
                SearchHit {
                    record,
                    distance: -score,
                }
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| left.distance.total_cmp(&right.distance));
        hits.truncate(top_k);
        Ok(hits)
    }

    pub async fn forget(&self, id: MemoryId) -> Result<()> {
        let inner = self.inner.clone();
        let backend = self.vector_backend.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::delete_from_backend(&backend, &[id])?;
            let tx = conn.transaction()?;
            Self::delete_ids_for_backend(&tx, &[id], &backend)?;
            tx.commit()?;
            Self::reconcile_documents_locked(&mut conn)?;
            Ok(())
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn clear(&self) -> Result<usize> {
        let inner = self.inner.clone();
        let backend = self.vector_backend.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            let ids = Self::all_ids(&conn)?;
            Self::delete_from_backend(&backend, &ids)?;
            let tx = conn.transaction()?;
            let deleted: usize =
                tx.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
            match backend {
                VectorBackend::SqliteVec => tx.execute("DELETE FROM memory_vectors", [])?,
                VectorBackend::FocalVector { .. } => {
                    tx.execute("DELETE FROM memory_embeddings", [])?
                }
            };
            tx.execute("DELETE FROM memories", [])?;
            tx.execute("DELETE FROM indexed_documents", [])?;
            tx.commit()?;
            Ok(deleted)
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn status(&self) -> Result<MemoryStatus> {
        let inner = self.inner.clone();
        let policy = self.policy;
        let backend = self.vector_backend.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::prune_backend_locked(&mut conn, policy, now_unix(), &backend)?;
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
                vector_backend: match backend {
                    VectorBackend::SqliteVec => "sqlite-vec",
                    VectorBackend::FocalVector { .. } => "focal-vector",
                }
                .to_string(),
            })
        })
        .await
        .context("memory store task panicked")?
    }

    pub async fn save_document(&self, document: IndexedDocument) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");
            conn.execute(
                "INSERT INTO indexed_documents (
                    source, title, media_type, content_hash, modified_at_unix,
                    indexed_at_unix, chunk_count, memory_ids
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(source) DO UPDATE SET
                    title=excluded.title,
                    media_type=excluded.media_type,
                    content_hash=excluded.content_hash,
                    modified_at_unix=excluded.modified_at_unix,
                    indexed_at_unix=excluded.indexed_at_unix,
                    chunk_count=excluded.chunk_count,
                    memory_ids=excluded.memory_ids",
                params![
                    document.source,
                    document.title,
                    document.media_type,
                    document.content_hash,
                    document.modified_at_unix,
                    document.indexed_at_unix,
                    document.chunk_count as i64,
                    serde_json::to_string(&document.memory_ids)?,
                ],
            )?;
            Ok(())
        })
        .await
        .context("document catalog task panicked")?
    }

    pub async fn document(&self, source: &str) -> Result<Option<IndexedDocument>> {
        let inner = self.inner.clone();
        let source = source.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::reconcile_documents_locked(&mut conn)?;
            Self::document_locked(&conn, &source)
        })
        .await
        .context("document catalog task panicked")?
    }

    pub async fn documents(&self) -> Result<Vec<IndexedDocument>> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().expect("memory store connection poisoned");
            Self::reconcile_documents_locked(&mut conn)?;
            let mut stmt = conn.prepare(
                "SELECT source, title, media_type, content_hash, modified_at_unix,
                        indexed_at_unix, chunk_count, memory_ids
                 FROM indexed_documents ORDER BY title COLLATE NOCASE, source",
            )?;
            let rows = stmt
                .query_map([], document_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .context("document catalog task panicked")?
    }

    pub async fn delete_document(&self, source: String) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().expect("memory store connection poisoned");
            conn.execute(
                "DELETE FROM indexed_documents WHERE source = ?1",
                params![source],
            )?;
            Ok(())
        })
        .await
        .context("document catalog task panicked")?
    }

    fn prune_locked(
        conn: &mut Connection,
        policy: MemoryPolicy,
        now: i64,
        sqlite_vectors: bool,
    ) -> Result<usize> {
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
        deleted = deleted.saturating_add(if sqlite_vectors {
            Self::delete_ids(&tx, &expired_ids)?
        } else {
            Self::delete_metadata_ids(&tx, &expired_ids)?
        });

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
                deleted = deleted.saturating_add(if sqlite_vectors {
                    Self::delete_ids(&tx, &oldest_ids)?
                } else {
                    Self::delete_metadata_ids(&tx, &oldest_ids)?
                });
            }
        }

        tx.commit()?;
        Ok(deleted)
    }

    fn prune_backend_locked(
        conn: &mut Connection,
        policy: MemoryPolicy,
        now: i64,
        backend: &VectorBackend,
    ) -> Result<usize> {
        if matches!(backend, VectorBackend::SqliteVec) {
            return Self::prune_locked(conn, policy, now, true);
        }
        let ids = Self::expired_and_excess_ids(conn, policy, now)?;
        Self::delete_from_backend(backend, &ids)?;
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        let deleted = Self::delete_ids_for_backend(&tx, &ids, backend)?;
        tx.commit()?;
        Ok(deleted)
    }

    fn expired_and_excess_ids(
        conn: &Connection,
        policy: MemoryPolicy,
        now: i64,
    ) -> Result<Vec<MemoryId>> {
        let mut ids = {
            let mut stmt = conn.prepare(
                "SELECT id FROM memories
                 WHERE expires_at_unix IS NOT NULL AND expires_at_unix <= ?1",
            )?;
            let expired = stmt
                .query_map(params![now], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            expired
        };
        if let Some(max_entries) = policy.max_entries {
            let remaining: usize = conn.query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE expires_at_unix IS NULL OR expires_at_unix > ?1",
                params![now],
                |row| row.get(0),
            )?;
            let excess = remaining.saturating_sub(max_entries);
            if excess > 0 {
                let mut stmt = conn.prepare(
                    "SELECT id FROM memories
                     WHERE expires_at_unix IS NULL OR expires_at_unix > ?1
                     ORDER BY created_at_unix ASC, id ASC LIMIT ?2",
                )?;
                let oldest = stmt
                    .query_map(params![now, excess as i64], |row| row.get::<_, MemoryId>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(oldest);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    fn recall_sqlite(
        conn: &Connection,
        query_embedding: Vec<f32>,
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.text, m.metadata, m.created_at_unix, v.distance
             FROM memory_vectors v
             JOIN memories m ON m.id = v.rowid
             WHERE v.embedding MATCH ?1 AND k = ?2
             ORDER BY v.distance",
        )?;
        let rows = stmt.query_map(params![query_embedding.as_bytes(), top_k as i64], |row| {
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
    }

    fn recall_lexical(conn: &Connection, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        let fts_query = terms(query)
            .into_iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT m.id, m.text, m.metadata, m.created_at_unix, bm25(memory_fts)
             FROM memory_fts
             JOIN memories m ON m.id = memory_fts.rowid
             WHERE memory_fts MATCH ?1
             ORDER BY bm25(memory_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, top_k as i64], |row| {
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
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn document_locked(conn: &Connection, source: &str) -> Result<Option<IndexedDocument>> {
        let mut stmt = conn.prepare(
            "SELECT source, title, media_type, content_hash, modified_at_unix,
                    indexed_at_unix, chunk_count, memory_ids
             FROM indexed_documents WHERE source = ?1",
        )?;
        let mut rows = stmt.query(params![source])?;
        rows.next()?
            .map(document_from_row)
            .transpose()
            .map_err(Into::into)
    }

    fn reconcile_documents_locked(conn: &mut Connection) -> Result<()> {
        let catalog = {
            let mut stmt = conn.prepare("SELECT source, memory_ids FROM indexed_documents")?;
            let documents = stmt
                .query_map([], |row| {
                    let ids: String = row.get(1)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        serde_json::from_str::<Vec<MemoryId>>(&ids).unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            documents
        };
        let mut changes = Vec::new();
        {
            let mut exists = conn.prepare("SELECT EXISTS(SELECT 1 FROM memories WHERE id = ?1)")?;
            for (source, ids) in catalog {
                let mut live = Vec::with_capacity(ids.len());
                for id in &ids {
                    if exists.query_row(params![id], |row| row.get::<_, bool>(0))? {
                        live.push(*id);
                    }
                }
                if live != ids {
                    changes.push((source, live));
                }
            }
        }
        let tx = conn.transaction()?;
        for (source, live) in changes {
            if live.is_empty() {
                tx.execute(
                    "DELETE FROM indexed_documents WHERE source = ?1",
                    params![source],
                )?;
            } else {
                tx.execute(
                    "UPDATE indexed_documents SET chunk_count = ?2, memory_ids = ?3 WHERE source = ?1",
                    params![source, live.len() as i64, serde_json::to_string(&live)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn record_by_id(conn: &Connection, id: MemoryId) -> Result<Option<MemoryRecord>> {
        let mut stmt =
            conn.prepare("SELECT id, text, metadata, created_at_unix FROM memories WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let metadata: String = row.get(2)?;
        Ok(Some(MemoryRecord {
            id: row.get(0)?,
            text: row.get(1)?,
            metadata: serde_json::from_str(&metadata).unwrap_or(Value::Null),
            created_at_unix: row.get(3)?,
        }))
    }

    fn all_ids(conn: &Connection) -> Result<Vec<MemoryId>> {
        let mut stmt = conn.prepare("SELECT id FROM memories ORDER BY id")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    fn import_legacy_embeddings(conn: &mut Connection) -> Result<()> {
        let has_legacy: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_vectors')",
            [],
            |row| row.get(0),
        )?;
        if !has_legacy {
            return Ok(());
        }
        let legacy = {
            let mut stmt = conn.prepare(
                "SELECT rowid, embedding FROM memory_vectors
                 WHERE rowid NOT IN (SELECT memory_id FROM memory_embeddings)",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, MemoryId>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let tx = conn.transaction()?;
        for (id, embedding) in legacy {
            tx.execute(
                "INSERT OR IGNORE INTO memory_embeddings (memory_id, embedding, indexed)
                 VALUES (?1, ?2, 0)",
                params![id, embedding],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn import_focal_embeddings_to_sqlite(conn: &mut Connection) -> Result<()> {
        let embeddings = {
            let mut stmt = conn.prepare(
                "SELECT memory_id, embedding FROM memory_embeddings
                 WHERE memory_id NOT IN (SELECT rowid FROM memory_vectors)",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, MemoryId>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let tx = conn.transaction()?;
        for (id, embedding) in embeddings {
            tx.execute(
                "INSERT OR IGNORE INTO memory_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![id, embedding],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn sync_pending_embeddings(conn: &mut Connection, backend: &VectorBackend) -> Result<()> {
        let VectorBackend::FocalVector { client, collection } = backend else {
            return Ok(());
        };
        loop {
            let pending = {
                let mut stmt = conn.prepare(
                    "SELECT e.memory_id, e.embedding, m.metadata
                     FROM memory_embeddings e
                     JOIN memories m ON m.id = e.memory_id
                     WHERE e.indexed = 0 ORDER BY e.memory_id LIMIT 256",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, MemoryId>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            if pending.is_empty() {
                break;
            }
            let mut ids = Vec::with_capacity(pending.len());
            let mut points = Vec::with_capacity(pending.len());
            for (id, bytes, metadata_json) in pending {
                let vector = decode_embedding(&bytes)?;
                let metadata_value: Value =
                    serde_json::from_str(&metadata_json).unwrap_or(Value::Null);
                let mut metadata = BTreeMap::new();
                metadata.insert("memory_id".into(), Value::from(id));
                if let Value::Object(fields) = metadata_value {
                    for (key, value) in fields {
                        if matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                            metadata.insert(format!("meta_{key}"), value);
                        }
                    }
                }
                ids.push(id);
                points.push(Point {
                    id: id.to_string(),
                    vector,
                    metadata,
                });
            }
            client
                .upsert(collection.clone(), points)
                .context("failed to synchronize memories with Focal Vector")?;
            let tx = conn.transaction()?;
            for id in ids {
                tx.execute(
                    "UPDATE memory_embeddings SET indexed = 1 WHERE memory_id = ?1",
                    params![id],
                )?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    fn delete_from_backend(backend: &VectorBackend, ids: &[MemoryId]) -> Result<()> {
        let VectorBackend::FocalVector { client, collection } = backend else {
            return Ok(());
        };
        for batch in ids.chunks(1_000) {
            if batch.is_empty() {
                continue;
            }
            client
                .delete(
                    collection.clone(),
                    batch.iter().map(ToString::to_string).collect(),
                )
                .context("failed to delete memories from Focal Vector")?;
        }
        Ok(())
    }

    fn delete_ids_for_backend(
        tx: &Transaction<'_>,
        ids: &[MemoryId],
        backend: &VectorBackend,
    ) -> Result<usize> {
        match backend {
            VectorBackend::SqliteVec => Self::delete_ids(tx, ids),
            VectorBackend::FocalVector { .. } => Self::delete_metadata_ids(tx, ids),
        }
    }

    fn delete_metadata_ids(tx: &Transaction<'_>, ids: &[MemoryId]) -> Result<usize> {
        for id in ids {
            tx.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        }
        Ok(ids.len())
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

fn protect_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        if parent.file_name().is_some_and(|name| name == "focaldesk") {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to protect {}", parent.display()))?;
        }
    }
    Ok(())
}

fn decode_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(std::mem::size_of::<f32>()) {
        bail!("stored embedding has invalid byte length {}", bytes.len());
    }
    Ok(bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("four-byte chunk")))
        .collect())
}

fn document_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedDocument> {
    let memory_ids: String = row.get(7)?;
    Ok(IndexedDocument {
        source: row.get(0)?,
        title: row.get(1)?,
        media_type: row.get(2)?,
        content_hash: row.get(3)?,
        modified_at_unix: row.get(4)?,
        indexed_at_unix: row.get(5)?,
        chunk_count: row.get::<_, i64>(6)?.max(0) as usize,
        memory_ids: serde_json::from_str(&memory_ids).unwrap_or_default(),
    })
}

fn terms(text: &str) -> HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map(str::to_lowercase)
        .filter(|term| term.chars().count() >= 2)
        .collect()
}

fn wait_for_focal_vector(client: &FocalVectorClient) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match client.hello() {
            Ok(_) => return Ok(()),
            Err(error) if std::time::Instant::now() < deadline => {
                tracing::debug!(
                    target: "focaldesk.memory",
                    %error,
                    "waiting for Focal Vector sidecar"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error).context("Focal Vector sidecar is unavailable"),
        }
    }
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
        assert_eq!(status.schema_version, 4);
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
        assert_eq!(version, 4);
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
                 PRAGMA user_version = 5;",
            )
            .unwrap();
        }

        let error = MemoryStore::open(&path, 4)
            .err()
            .expect("newer schema must fail closed");
        assert!(error.to_string().contains("newer than supported version 4"));

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
        assert_eq!(version, 5);
        assert_eq!(sentinel, "untouched");
        assert!(!memories_table_exists);
        drop(conn);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn orphaned_embeddings_are_removed_when_the_store_opens() {
        let path = test_path("orphaned-embedding");
        {
            let store = MemoryStore::open(&path, 4).unwrap();
            let id = test_runtime()
                .block_on(store.remember(
                    "stale memory".into(),
                    vec![1.0, 0.0, 0.0, 0.0],
                    Value::Null,
                ))
                .unwrap();
            assert_eq!(id, 1);
            drop(store);

            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO memory_embeddings (memory_id, embedding, indexed)
                 VALUES (1, ?1, 0)",
                params![vec![0_u8; 16]],
            )
            .unwrap();
            conn.execute("DELETE FROM memories WHERE id = 1", [])
                .unwrap();
        }

        let store = MemoryStore::open(&path, 4).unwrap();
        let id = test_runtime()
            .block_on(store.remember("new memory".into(), vec![1.0, 0.0, 0.0, 0.0], Value::Null))
            .unwrap();
        assert_eq!(id, 1);
        drop(store);
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

    #[test]
    fn hybrid_recall_promotes_exact_lexical_matches() {
        let path = test_path("hybrid-recall");
        let store = MemoryStore::open(&path, 4).unwrap();
        test_runtime().block_on(async {
            store
                .remember(
                    "generic semantic neighbor".into(),
                    vec![1.0, 0.0, 0.0, 0.0],
                    Value::Null,
                )
                .await
                .unwrap();
            store
                .remember(
                    "diagnostic code ZXQ441 identifies the failure".into(),
                    vec![0.0, 1.0, 0.0, 0.0],
                    Value::Null,
                )
                .await
                .unwrap();

            let hits = store
                .recall_hybrid("ZXQ441 failure".into(), vec![1.0, 0.0, 0.0, 0.0], 1)
                .await
                .unwrap();
            assert_eq!(
                hits[0].record.text,
                "diagnostic code ZXQ441 identifies the failure"
            );
        });
        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn indexed_document_catalog_round_trips_and_deletes() {
        let path = test_path("document-catalog");
        let store = MemoryStore::open(&path, 4).unwrap();
        test_runtime().block_on(async {
            let first_id = store
                .remember("first chunk".into(), vec![1.0, 0.0, 0.0, 0.0], Value::Null)
                .await
                .unwrap();
            let second_id = store
                .remember("second chunk".into(), vec![0.0, 1.0, 0.0, 0.0], Value::Null)
                .await
                .unwrap();
            let document = IndexedDocument {
                source: "/tmp/guide.pdf".into(),
                title: "guide.pdf".into(),
                media_type: "application/pdf".into(),
                content_hash: "abc123".into(),
                modified_at_unix: 10,
                indexed_at_unix: 20,
                chunk_count: 2,
                memory_ids: vec![first_id, second_id],
            };
            store.save_document(document.clone()).await.unwrap();
            assert_eq!(
                store.document(&document.source).await.unwrap(),
                Some(document)
            );
            assert_eq!(store.documents().await.unwrap().len(), 1);
            store.forget(first_id).await.unwrap();
            let reconciled = store.documents().await.unwrap();
            assert_eq!(reconciled[0].chunk_count, 1);
            assert_eq!(reconciled[0].memory_ids, vec![second_id]);
            store.clear().await.unwrap();
            assert!(store.documents().await.unwrap().is_empty());
        });
        drop(store);
        let _ = fs::remove_file(path);
    }
}
