use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{AgentRequest, AgentRunStatus};

const AGENT_RUN_SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub(crate) struct AgentRunStore {
    connection: Arc<Mutex<Connection>>,
}

impl AgentRunStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let parent_existed = parent.exists();
            if parent_existed && fs::symlink_metadata(parent)?.file_type().is_symlink() {
                bail!("agent run database directory must not be a symlink");
            }
            fs::create_dir_all(parent)
                .with_context(|| format!("create agent run directory {}", parent.display()))?;
            if !parent_existed || parent.file_name().is_some_and(|name| name == "focaldesk") {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("protect agent run directory {}", parent.display()))?;
            }
        }
        if path.exists() {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!("agent run database must be a regular file");
            }
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open agent run database {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("protect agent run database {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > AGENT_RUN_SCHEMA_VERSION {
            bail!(
                "agent run database schema {version} is newer than supported schema {AGENT_RUN_SCHEMA_VERSION}"
            );
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_runs (
                run_id TEXT PRIMARY KEY,
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL,
                status_json TEXT NOT NULL,
                request_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS agent_runs_created_idx
                ON agent_runs(created_at_unix DESC);
            CREATE TABLE IF NOT EXISTS agent_trigger_firings (
                run_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                trigger_id TEXT NOT NULL,
                trigger_kind TEXT NOT NULL,
                fired_at_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS agent_trigger_firings_lookup_idx
                ON agent_trigger_firings(agent_id, trigger_id, trigger_kind, fired_at_unix DESC);
            CREATE TABLE IF NOT EXISTS agent_runtime_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_usage_ledger (
                run_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                completed_at_unix INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS agent_usage_lookup_idx
                ON agent_usage_ledger(agent_id, completed_at_unix DESC);
            CREATE TABLE IF NOT EXISTS agent_control_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                at_unix INTEGER NOT NULL,
                agent_id TEXT,
                action TEXT NOT NULL,
                details TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_runs (
                run_id TEXT PRIMARY KEY,
                created_at_unix INTEGER NOT NULL,
                status_json TEXT NOT NULL
            );",
        )?;
        connection.pragma_update(None, "user_version", AGENT_RUN_SCHEMA_VERSION)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn load(&self, limit: usize) -> Result<Vec<(AgentRunStatus, AgentRequest)>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let mut statement = connection.prepare(
            "SELECT status_json, request_json FROM agent_runs
             ORDER BY created_at_unix DESC, run_id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (status, request) = row?;
            records.push((
                serde_json::from_str(&status).context("decode persisted agent run status")?,
                serde_json::from_str(&request).context("decode persisted agent request")?,
            ));
        }
        Ok(records)
    }

    pub(crate) fn save(&self, status: &AgentRunStatus, request: &AgentRequest) -> Result<()> {
        let status_json = serde_json::to_string(status).context("encode agent run status")?;
        let request_json = serde_json::to_string(request).context("encode agent request")?;
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute(
                "INSERT INTO agent_runs (
                    run_id, created_at_unix, updated_at_unix, status_json, request_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(run_id) DO UPDATE SET
                    updated_at_unix = excluded.updated_at_unix,
                    status_json = excluded.status_json,
                    request_json = excluded.request_json",
                params![
                    status.run_id,
                    status.created_at_unix,
                    crate::service::unix_now(),
                    status_json,
                    request_json
                ],
            )?;
        if let (Some(completed_at), Some(usage)) = (
            status.completed_at_unix,
            status.result.as_ref().and_then(|result| result.usage),
        ) {
            let mut connection = self
                .connection
                .lock()
                .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM agent_usage_ledger WHERE completed_at_unix < ?1",
                [completed_at.saturating_sub(90 * 86_400)],
            )?;
            transaction.execute(
                "INSERT INTO agent_usage_ledger (
                        run_id, agent_id, completed_at_unix, input_tokens, output_tokens
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(run_id) DO UPDATE SET
                        completed_at_unix = excluded.completed_at_unix,
                        input_tokens = excluded.input_tokens,
                        output_tokens = excluded.output_tokens",
                params![
                    status.run_id,
                    status.agent_id,
                    completed_at,
                    usage.input_tokens,
                    usage.output_tokens
                ],
            )?;
            transaction.commit()?;
        }
        Ok(())
    }

    pub(crate) fn delete(&self, run_id: &str) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute("DELETE FROM agent_runs WHERE run_id = ?1", [run_id])?;
        Ok(())
    }

    pub(crate) fn trigger_firings(
        &self,
        agent_id: &str,
        trigger_id: &str,
        trigger_kind: &str,
        since_unix: u64,
    ) -> Result<Vec<u64>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let mut statement = connection.prepare(
            "SELECT fired_at_unix FROM agent_trigger_firings
             WHERE agent_id = ?1 AND trigger_id = ?2 AND trigger_kind = ?3
               AND fired_at_unix >= ?4
             ORDER BY fired_at_unix DESC",
        )?;
        Ok(statement
            .query_map(
                params![agent_id, trigger_id, trigger_kind, since_unix],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(crate) fn record_trigger_firing(
        &self,
        run_id: &str,
        agent_id: &str,
        trigger_id: &str,
        trigger_kind: &str,
        fired_at_unix: u64,
    ) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM agent_trigger_firings WHERE fired_at_unix < ?1",
            [fired_at_unix.saturating_sub(86_400)],
        )?;
        transaction.execute(
            "INSERT INTO agent_trigger_firings (
                run_id, agent_id, trigger_id, trigger_kind, fired_at_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, agent_id, trigger_id, trigger_kind, fired_at_unix],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn triggers_suspended(&self) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let value = connection.query_row(
            "SELECT value FROM agent_runtime_meta WHERE key = 'triggers_suspended'",
            [],
            |row| row.get::<_, String>(0),
        );
        match value {
            Ok(value) => Ok(value == "true"),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn set_triggers_suspended(&self, suspended: bool) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute(
                "INSERT INTO agent_runtime_meta (key, value) VALUES ('triggers_suspended', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [if suspended { "true" } else { "false" }],
            )?;
        Ok(())
    }

    pub(crate) fn mission_control_paused(&self) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let value = connection.query_row(
            "SELECT value FROM agent_runtime_meta WHERE key = 'mission_control_paused'",
            [],
            |row| row.get::<_, String>(0),
        );
        match value {
            Ok(value) => Ok(value == "true"),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn set_mission_control_paused(&self, paused: bool) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute(
                "INSERT INTO agent_runtime_meta (key, value) VALUES ('mission_control_paused', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [if paused { "true" } else { "false" }],
            )?;
        Ok(())
    }

    pub(crate) fn agent_enabled(&self, agent_id: &str) -> Result<bool> {
        Ok(self.agent_enabled_override(agent_id)?.unwrap_or(true))
    }

    pub(crate) fn agent_enabled_override(&self, agent_id: &str) -> Result<Option<bool>> {
        let key = format!("agent_disabled:{agent_id}");
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        match connection.query_row(
            "SELECT value FROM agent_runtime_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(value) => Ok(Some(value != "true")),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn set_agent_enabled(&self, agent_id: &str, enabled: bool) -> Result<()> {
        let key = format!("agent_disabled:{agent_id}");
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute(
                "INSERT INTO agent_runtime_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, if enabled { "false" } else { "true" }],
            )?;
        Ok(())
    }

    pub(crate) fn set_agents_enabled(&self, agent_ids: &[String], enabled: bool) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let transaction = connection.transaction()?;
        for agent_id in agent_ids {
            let key = format!("agent_disabled:{agent_id}");
            transaction.execute(
                "INSERT INTO agent_runtime_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, if enabled { "false" } else { "true" }],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn daily_usage(&self, agent_id: &str, since_unix: u64) -> Result<(u64, u64)> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
                 FROM agent_usage_ledger WHERE agent_id = ?1 AND completed_at_unix >= ?2",
                params![agent_id, since_unix],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
    }

    pub(crate) fn record_usage(
        &self,
        usage_id: &str,
        agent_id: &str,
        completed_at_unix: u64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM agent_usage_ledger WHERE completed_at_unix < ?1",
            [completed_at_unix.saturating_sub(90 * 86_400)],
        )?;
        transaction.execute(
            "INSERT INTO agent_usage_ledger (
                    run_id, agent_id, completed_at_unix, input_tokens, output_tokens
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                usage_id,
                agent_id,
                completed_at_unix,
                input_tokens,
                output_tokens
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn record_control_event(
        &self,
        agent_id: Option<&str>,
        action: &str,
        details: &str,
    ) -> Result<()> {
        if details.chars().count() > 500 {
            bail!("agent control audit details exceed 500 characters");
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO agent_control_events (at_unix, agent_id, action, details)
                 VALUES (?1, ?2, ?3, ?4)",
            params![crate::service::unix_now(), agent_id, action, details],
        )?;
        transaction.execute(
            "DELETE FROM agent_control_events WHERE sequence NOT IN (
                SELECT sequence FROM agent_control_events ORDER BY sequence DESC LIMIT 2048
             )",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn load_control_events(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::mission::MissionAuditRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let mut statement = connection.prepare(
            "SELECT sequence, at_unix, agent_id, action, details
             FROM agent_control_events ORDER BY sequence DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.min(2048) as i64], |row| {
            Ok(crate::mission::MissionAuditRecord {
                sequence: row.get(0)?,
                at_unix: row.get(1)?,
                agent_id: row.get(2)?,
                action: row.get(3)?,
                details: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(crate) fn load_workflows(&self, limit: usize) -> Result<Vec<crate::WorkflowRunStatus>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?;
        let mut statement = connection.prepare(
            "SELECT status_json FROM workflow_runs
             ORDER BY created_at_unix DESC, run_id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| row.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).context("decode persisted workflow run status"))
            .collect()
    }

    pub(crate) fn save_workflow(&self, status: &crate::WorkflowRunStatus) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute(
                "INSERT INTO workflow_runs (run_id, created_at_unix, status_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET status_json = excluded.status_json",
                params![
                    status.run_id,
                    status.created_at_unix,
                    serde_json::to_string(status)?
                ],
            )?;
        Ok(())
    }

    pub(crate) fn delete_workflow(&self, run_id: &str) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("agent run database is unavailable"))?
            .execute("DELETE FROM workflow_runs WHERE run_id = ?1", [run_id])?;
        Ok(())
    }
}
