use crate::Message;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use std::{path::PathBuf, time::Duration};

#[path = "store_actions.rs"]
mod actions;
#[path = "store_graph.rs"]
mod graph;
#[path = "store_search.rs"]
mod search;
#[path = "store_sync.rs"]
mod sync;
pub struct Store(Connection);
impl Store {
    pub fn open() -> Result<Self> {
        let path = dirs::data_local_dir()
            .context("Cannot locate user data directory")?
            .join("focalmail");
        std::fs::create_dir_all(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Self::at(path.join("mail.sqlite3"))
    }
    fn at(path: PathBuf) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }
    pub(crate) fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(30))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // WAL lets the daemon commit downloads while the window holds a read snapshot.
        // Checking first avoids taking a journal-mode write lock on every read connection.
        let journal: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        if journal != "wal" && journal != "memory" {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        let has_migrations: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='mail_migrations')",
            [], |r| r.get(0),
        )?;
        if has_migrations {
            let ready: bool = conn.query_row(
                "SELECT count(*)=10 FROM mail_migrations WHERE version IN (1,2,3,4,5,6,7,8,9,10)",
                [],
                |r| r.get(0),
            )?;
            if ready {
                // Opening the cache for a mailbox/status read must not acquire a writer.
                return Ok(Self(conn));
            }
        }
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS accounts (email TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS account_status (email TEXT PRIMARY KEY, state TEXT NOT NULL, detail TEXT NOT NULL, updated INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS messages (
                account TEXT NOT NULL REFERENCES accounts(email), id TEXT NOT NULL,
                position INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,id));
            CREATE TABLE IF NOT EXISTS sync_state (account TEXT PRIMARY KEY REFERENCES accounts(email), last_sync INTEGER NOT NULL, history_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS account_profiles (email TEXT NOT NULL, provider TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(email,provider));
            CREATE TABLE IF NOT EXISTS drafts (id INTEGER PRIMARY KEY CHECK(id=1), recipient TEXT NOT NULL, subject TEXT NOT NULL, body TEXT NOT NULL);")?;
        // The prototype cursor covered only 100 inbox messages. Invalidate it once
        // so the first upgraded refresh builds a complete mailbox before using history.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS mail_migrations (version INTEGER PRIMARY KEY);
            BEGIN IMMEDIATE;
            UPDATE sync_state SET history_id='' WHERE NOT EXISTS (SELECT 1 FROM mail_migrations WHERE version=1);
            INSERT OR IGNORE INTO mail_migrations VALUES(1);
            COMMIT;
            CREATE TABLE IF NOT EXISTS draft_sender (id INTEGER PRIMARY KEY CHECK(id=1), account TEXT NOT NULL);")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS draft_attachments (position INTEGER PRIMARY KEY, name TEXT NOT NULL, data BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS full_sync_state (account TEXT PRIMARY KEY REFERENCES accounts(email), history TEXT NOT NULL, page TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS full_sync_messages (account TEXT NOT NULL REFERENCES accounts(email), id TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,id));")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS connected_providers (email TEXT PRIMARY KEY REFERENCES accounts(email), profile TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS mail_preferences (name TEXT PRIMARY KEY, value INTEGER NOT NULL);")?;
        let mut store = Self(conn);
        store.0.execute_batch("CREATE TABLE IF NOT EXISTS local_drafts (id TEXT PRIMARY KEY, data TEXT NOT NULL, updated INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS local_draft_files (draft_id TEXT NOT NULL REFERENCES local_drafts(id) ON DELETE CASCADE, position INTEGER NOT NULL, name TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY(draft_id,position));
            CREATE TABLE IF NOT EXISTS active_local_draft (slot INTEGER PRIMARY KEY CHECK(slot=1), draft_id TEXT NOT NULL REFERENCES local_drafts(id) ON DELETE CASCADE);")?;
        store.migrate_drafts()?;
        store.migrate_mailbox()?;
        store.0.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS mail_sync_progress (account TEXT PRIMARY KEY,phase TEXT NOT NULL,completed INTEGER NOT NULL,total INTEGER,updated INTEGER NOT NULL);
            INSERT OR IGNORE INTO mail_migrations VALUES(4);
            COMMIT;")?;
        store.migrate_search()?;
        store.migrate_sync_history()?;
        store.0.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS account_folders(account TEXT NOT NULL REFERENCES accounts(email) ON DELETE CASCADE,folder TEXT NOT NULL,PRIMARY KEY(account,folder));
            INSERT OR IGNORE INTO mail_migrations VALUES(7);
            COMMIT;")?;
        store.migrate_graph()?;
        store.0.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS mail_signatures (account TEXT PRIMARY KEY, data TEXT NOT NULL);
            INSERT OR IGNORE INTO mail_migrations VALUES(10);
            COMMIT;")?;
        Ok(store)
    }
    // Triggers also cover writes from an already-running older mail service.
    // Keep list metadata separate so paging never scans large MIME documents.
    fn migrate_mailbox(&mut self) -> Result<()> {
        let migrated: bool = self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=3)",
            [],
            |r| r.get(0),
        )?;
        if migrated {
            return Ok(());
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let migrated: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=3)",
            [],
            |r| r.get(0),
        )?;
        if migrated {
            tx.commit()?;
            return Ok(());
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS mailbox_summaries (
            account TEXT NOT NULL,id TEXT NOT NULL,staged INTEGER NOT NULL,data TEXT NOT NULL,
            timestamp INTEGER NOT NULL,inbox INTEGER NOT NULL,sent INTEGER NOT NULL,
            unread INTEGER NOT NULL,flagged INTEGER NOT NULL,folder TEXT NOT NULL,
            PRIMARY KEY(account,id,staged));
            CREATE INDEX IF NOT EXISTS mailbox_summaries_date ON mailbox_summaries(timestamp DESC,account,id);")?;
        for (table, staged) in [("messages", 0), ("full_sync_messages", 1)] {
            let columns = |source: &str| {
                let data = format!("{source}.data");
                let visible = format!("NOT EXISTS(SELECT 1 FROM json_each({data},'$.labels') WHERE value IN ('TRASH','SPAM','DRAFT'))");
                let folder = |label: &str, name: &str| {
                    format!("((coalesce(json_array_length(json_extract({data},'$.labels')),0)=0 AND json_extract({data},'$.folder')='{name}') OR (EXISTS(SELECT 1 FROM json_each({data},'$.labels') WHERE value='{label}') AND {visible}))")
                };
                format!("{source}.account,{source}.id,{staged},json_set({data},'$.body',substr(json_extract({data},'$.body'),1,160),'$.html',NULL,'$.attachments',json('[]')),coalesce(json_extract({data},'$.timestamp'),0),{},{},coalesce(json_extract({data},'$.unread'),0),coalesce(json_extract({data},'$.flagged'),0),coalesce(json_extract({data},'$.folder'),'')",folder("INBOX","Inbox"),folder("SENT","Sent"))
            };
            for event in ["INSERT", "UPDATE"] {
                tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS mailbox_{table}_{event} AFTER {event} ON {table} BEGIN INSERT OR REPLACE INTO mailbox_summaries SELECT {}; END;", columns("new")))?;
            }
            tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS mailbox_{table}_delete AFTER DELETE ON {table} BEGIN DELETE FROM mailbox_summaries WHERE account=old.account AND id=old.id AND staged={staged}; END;"))?;
            tx.execute_batch(&format!(
                "INSERT OR IGNORE INTO mailbox_summaries SELECT {} FROM {table} source;",
                columns("source")
            ))?;
        }
        tx.execute("INSERT OR IGNORE INTO mail_migrations VALUES(3)", [])?;
        tx.commit()?;
        Ok(())
    }

    fn migrate_drafts(&mut self) -> Result<()> {
        let migrated: bool = self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=2)",
            [],
            |r| r.get(0),
        )?;
        if migrated {
            return Ok(());
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let migrated: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=2)",
            [],
            |r| r.get(0),
        )?;
        if !migrated {
            let (to, subject, body): (String, String, String) = {
                use rusqlite::OptionalExtension;
                tx.query_row(
                    "SELECT recipient,subject,body FROM drafts WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .unwrap_or_default()
            };
            let has_files: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM draft_attachments)", [], |r| {
                    r.get(0)
                })?;
            if !to.is_empty() || !subject.is_empty() || !body.is_empty() || has_files {
                use rusqlite::OptionalExtension;
                let from = tx
                    .query_row("SELECT account FROM draft_sender WHERE id=1", [], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .unwrap_or_default();
                let draft = crate::drafts::Draft {
                    id: "migrated-local-draft".into(),
                    from,
                    to,
                    subject,
                    body,
                    ..Default::default()
                };
                tx.execute(
                    "INSERT OR IGNORE INTO local_drafts VALUES(?1,?2,unixepoch())",
                    params![draft.id, serde_json::to_string(&draft)?],
                )?;
                tx.execute("INSERT OR IGNORE INTO local_draft_files SELECT 'migrated-local-draft',position,name,data FROM draft_attachments", [])?;
            }
            tx.execute("DELETE FROM drafts", [])?;
            tx.execute("DELETE FROM draft_attachments", [])?;
            tx.execute("INSERT INTO mail_migrations VALUES(2)", [])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn active_draft_id(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT draft_id FROM active_local_draft WHERE slot=1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn list_drafts(&self) -> Result<Vec<crate::drafts::Summary>> {
        let data = self
            .0
            .prepare("SELECT data FROM local_drafts ORDER BY updated DESC,id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.into_iter()
            .map(|data| {
                let d: crate::drafts::Draft = serde_json::from_str(&data)?;
                Ok(crate::drafts::Summary {
                    id: d.id,
                    subject: d.subject,
                    to: d.to,
                    from: d.from,
                })
            })
            .collect()
    }
    pub fn load_local_draft(
        &self,
        id: &str,
    ) -> Result<(
        crate::drafts::Draft,
        Vec<crate::attachments::DraftAttachment>,
    )> {
        let data: String =
            self.0
                .query_row("SELECT data FROM local_drafts WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
        let files = self
            .0
            .prepare("SELECT name,data FROM local_draft_files WHERE draft_id=?1 ORDER BY position")?
            .query_map([id], |r| {
                Ok(crate::attachments::DraftAttachment {
                    name: r.get(0)?,
                    data: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok((serde_json::from_str(&data)?, files))
    }
    pub fn save_local_draft(
        &mut self,
        draft: &crate::drafts::Draft,
        files: Option<&[crate::attachments::DraftAttachment]>,
    ) -> Result<()> {
        anyhow::ensure!(!draft.id.is_empty(), "Draft ID is missing");
        if let Some(files) = files {
            anyhow::ensure!(
                files.iter().map(|f| f.data.len()).sum::<usize>() <= crate::attachments::MAX_BYTES,
                "Attachments may total at most 20 MiB"
            );
        }
        anyhow::ensure!(
            draft.body.len() <= 1024 * 1024
                && draft
                    .html_body
                    .as_ref()
                    .is_none_or(|s| s.len() <= 1024 * 1024),
            "Message text and HTML may each be at most 1 MiB"
        );
        let mut draft = draft.clone();
        draft.html_body = draft.html_body.as_deref().map(crate::compose::sanitize);
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO local_drafts VALUES(?1,?2,unixepoch()) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated=unixepoch()", params![draft.id,serde_json::to_string(&draft)?])?;
        if let Some(files) = files {
            tx.execute(
                "DELETE FROM local_draft_files WHERE draft_id=?1",
                [&draft.id],
            )?;
            for (index, file) in files.iter().enumerate() {
                tx.execute(
                    "INSERT INTO local_draft_files VALUES(?1,?2,?3,?4)",
                    params![draft.id, index, file.name, file.data],
                )?;
            }
        }
        tx.execute("INSERT INTO active_local_draft VALUES(1,?1) ON CONFLICT(slot) DO UPDATE SET draft_id=excluded.draft_id", [&draft.id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn delete_local_draft(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM local_drafts WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn sync_progress(
        &self,
        account: &str,
        phase: &str,
        completed: usize,
        total: Option<usize>,
    ) -> Result<()> {
        self.0.execute("INSERT INTO mail_sync_progress VALUES(?1,?2,?3,?4,unixepoch()) ON CONFLICT(account) DO UPDATE SET phase=excluded.phase,completed=excluded.completed,total=CASE WHEN excluded.total IS NOT NULL THEN excluded.total WHEN excluded.phase=mail_sync_progress.phase OR (mail_sync_progress.phase IN ('paused','error','connecting') AND excluded.phase='importing') THEN mail_sync_progress.total ELSE NULL END,updated=unixepoch()", params![account,phase,completed as i64,total.map(|n|n as i64)])?;
        Ok(())
    }
    pub fn background_accounts(&self) -> Result<Vec<String>> {
        Ok(self.0.prepare("SELECT email FROM accounts WHERE NOT EXISTS(SELECT 1 FROM account_status s WHERE s.email=accounts.email AND s.state='reconnect_required') ORDER BY email")?
            .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn recover_interrupted_sync(&self) -> Result<()> {
        self.0.execute_batch("BEGIN IMMEDIATE;
            UPDATE mail_sync_progress SET phase='paused',updated=unixepoch() WHERE account IN (SELECT email FROM account_status WHERE state='syncing');
            UPDATE account_status SET state='idle',detail='',updated=unixepoch() WHERE state='syncing';
            COMMIT;")?;
        Ok(())
    }
    pub fn record_sync_started(&self, email: &str) -> Result<()> {
        self.0.execute("INSERT INTO account_status VALUES(?1,'syncing','',unixepoch()) ON CONFLICT(email) DO UPDATE SET state='syncing',detail='',updated=unixepoch()",[email])?;
        self.0.execute(
            "UPDATE mail_sync_progress SET phase='connecting',updated=unixepoch() WHERE account=?1",
            [email],
        )?;
        Ok(())
    }
    pub fn cached_ids(&self, account: &str) -> Result<std::collections::HashSet<String>> {
        Ok(self.0.prepare("SELECT id FROM messages WHERE account=?1 UNION SELECT id FROM full_sync_messages WHERE account=?1")?
            .query_map([account], |r|r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn record_status(&self, email: &str, result: &Result<()>) -> Result<()> {
        let (state, detail) = match result {
            Ok(()) => ("connected", String::new()),
            Err(error) if error.is::<crate::service::OperationCancelled>() => {
                // Do not leave a cancelled scan looking active; preserve prior failures otherwise.
                self.0.execute("UPDATE account_status SET state='idle',detail='',updated=unixepoch() WHERE email=?1 AND state='syncing'",[email])?;
                self.0.execute("UPDATE mail_sync_progress SET phase='paused',updated=unixepoch() WHERE account=?1",[email])?;
                return Ok(());
            }
            Err(error) if crate::service::is_network_error(error) =>
            {
                (
                    "offline",
                    "The mail provider could not be reached or timed out. Sync will retry automatically; cached mail is available.".into(),
                )
            }
            Err(error) if error.is::<crate::service::ReconnectRequired>() => {
                ("reconnect_required", error.to_string())
            }
            Err(error) => ("sync_error", error.to_string()),
        };
        self.0.execute("INSERT INTO account_status VALUES(?1,?2,?3,unixepoch()) ON CONFLICT(email) DO UPDATE SET state=excluded.state,detail=excluded.detail,updated=excluded.updated", params![email,state,detail])?;
        self.0.execute(
            "UPDATE mail_sync_progress SET phase=?2,updated=unixepoch() WHERE account=?1",
            params![email, if result.is_ok() { "complete" } else { "error" }],
        )?;
        Ok(())
    }
    pub fn account_status(&self) -> Result<serde_json::Value> {
        let mut query = self.0.prepare("SELECT a.email,coalesce(s.state,'offline'),coalesce(s.detail,''),s.updated FROM accounts a LEFT JOIN account_status s ON a.email=s.email ORDER BY a.email")?;
        let accounts = query.query_map([], |r| Ok(serde_json::json!({"email":r.get::<_,String>(0)?, "provider":"gmail", "state":r.get::<_,String>(1)?, "detail":r.get::<_,String>(2)?, "updated":r.get::<_,Option<i64>>(3)?, "capabilities":["read","search","send","labels","attachments","reconnect"]})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut accounts = accounts;
        let sync_paused = self.sync_paused()?;
        let search_pending = self.search_pending()?;
        for account in &mut accounts {
            account["sync_paused"] = sync_paused.into();
            account["sync_history"] = serde_json::to_value(
                self.sync_history(account["email"].as_str().unwrap_or_default())?,
            )?;
            account["search_pending"] = search_pending.into();
            let email = account["email"].as_str().unwrap_or_default().to_owned();
            account["cached_count"] = self.0.query_row("SELECT count(*) FROM mailbox_summaries m WHERE account=?1 AND (staged=1 OR NOT EXISTS(SELECT 1 FROM mailbox_summaries f WHERE f.account=m.account AND f.id=m.id AND f.staged=1))",[&email],|r|r.get::<_,u64>(0))?.into();
            use rusqlite::OptionalExtension;
            account["progress"] = self.0.query_row("SELECT phase,completed,total,updated FROM mail_sync_progress WHERE account=?1",[&email],|r|Ok(serde_json::json!({"phase":r.get::<_,String>(0)?,"completed":r.get::<_,u64>(1)?,"total":r.get::<_,Option<u64>>(2)?,"updated":r.get::<_,i64>(3)?}))).optional()?.unwrap_or(serde_json::Value::Null);
            if let Some(profile) =
                self.connected_profile(account["email"].as_str().unwrap_or_default())?
            {
                account["provider"] = match profile.provider {
                    crate::settings::Provider::Microsoft365 => "outlook",
                    crate::settings::Provider::Manual => "imap",
                    _ => "gmail",
                }
                .into();
            }
        }
        Ok(serde_json::json!(accounts))
    }
    pub fn connected_profile(&self, email: &str) -> Result<Option<crate::settings::Profile>> {
        use rusqlite::OptionalExtension;
        let data: Option<String> = self
            .0
            .query_row(
                "SELECT profile FROM connected_providers WHERE email=?1",
                [email],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
    pub fn connect_profile(&mut self, profile: &crate::settings::Profile) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO accounts VALUES(?1)",
            [&profile.email],
        )?;
        tx.execute("INSERT INTO connected_providers VALUES(?1,?2) ON CONFLICT(email) DO UPDATE SET profile=excluded.profile", params![profile.email,serde_json::to_string(profile)?])?;
        tx.commit()?;
        self.save_profile(profile)
    }
    pub fn sync_paused(&self) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT value FROM mail_preferences WHERE name='sync_paused'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }
    pub fn set_sync_paused(&self, paused: bool) -> Result<()> {
        self.0.execute("INSERT INTO mail_preferences VALUES('sync_paused',?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value",[paused])?;
        Ok(())
    }
    pub fn signature(&self, account: &str) -> Result<crate::compose::Signature> {
        use rusqlite::OptionalExtension;
        let data: Option<String> = self
            .0
            .query_row(
                "SELECT data FROM mail_signatures WHERE account=?1",
                [account],
                |r| r.get(0),
            )
            .optional()?;
        Ok(data
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default())
    }
    pub fn set_signature(
        &self,
        account: &str,
        signature: &crate::compose::Signature,
    ) -> Result<()> {
        anyhow::ensure!(
            self.accounts()?.iter().any(|a| a == account),
            "Choose a connected account"
        );
        anyhow::ensure!(
            signature.html.len() <= 64 * 1024,
            "Signature may be at most 64 KiB"
        );
        let mut signature = signature.clone();
        signature.html = crate::compose::sanitize(&signature.html);
        self.0.execute("INSERT INTO mail_signatures VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET data=excluded.data", params![account, serde_json::to_string(&signature)?])?;
        Ok(())
    }
    pub fn notifications_enabled(&self) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT value FROM mail_preferences WHERE name='notifications'",
                [],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(true))
    }
    pub fn set_notifications(&self, enabled: bool) -> Result<()> {
        self.0.execute("INSERT INTO mail_preferences VALUES('notifications',?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value", [enabled])?;
        Ok(())
    }
    pub fn accounts(&self) -> Result<Vec<String>> {
        Ok(self
            .0
            .prepare("SELECT email FROM accounts ORDER BY email")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn profiles(&self) -> Result<Vec<crate::settings::Profile>> {
        let data = self
            .0
            .prepare("SELECT data FROM account_profiles ORDER BY email,provider")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }
    pub fn save_profile(&self, profile: &crate::settings::Profile) -> Result<()> {
        profile.validate()?;
        self.0.execute("INSERT INTO account_profiles VALUES(?1,?2,?3) ON CONFLICT(email,provider) DO UPDATE SET data=excluded.data", params![profile.email,serde_json::to_string(&profile.provider)?,serde_json::to_string(profile)?])?;
        Ok(())
    }
    pub fn account_folders(&self) -> Result<serde_json::Value> {
        let mut folders = std::collections::BTreeMap::<String, Vec<String>>::new();
        let mut statement = self.0.prepare("SELECT account,folder FROM account_folders UNION SELECT account,folder FROM mailbox_summaries WHERE folder!='' ORDER BY account,folder")?;
        for row in
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (account, folder) = row?;
            folders.entry(account).or_default().push(folder);
        }
        Ok(serde_json::to_value(folders)?)
    }
    pub fn replace_account_folders(&mut self, account: &str, folders: &[String]) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM account_folders WHERE account=?1", [account])?;
        for folder in folders {
            tx.execute(
                "INSERT OR IGNORE INTO account_folders VALUES(?1,?2)",
                params![account, folder],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Read only one mailbox page; never deserialize every cached MIME body for the UI.
    pub fn mailbox_page(
        &self,
        folder: &str,
        query: &str,
        offset: usize,
    ) -> Result<serde_json::Value> {
        // Account folder keys are JSON tuples, avoiding ambiguous delimiters in names.
        let scoped: Option<(String, String)> = folder
            .strip_prefix("account-folder:")
            .map(serde_json::from_str)
            .transpose()?;
        let account = scoped.as_ref().map(|(account, _)| account.as_str());
        let folder = scoped
            .as_ref()
            .map(|(_, folder)| folder.as_str())
            .unwrap_or(folder);
        let sql = "WITH cached AS (
            SELECT * FROM mailbox_summaries m WHERE m.staged=1 OR NOT EXISTS (
                SELECT 1 FROM mailbox_summaries f WHERE f.account=m.account AND f.id=m.id AND f.staged=1)
        ), filtered AS (
            SELECT data,timestamp,account,id FROM cached WHERE (:account IS NULL OR account=:account) AND (
                ((:folder IN ('All Inboxes','Inbox','Unread') OR account=:folder) AND inbox=1 AND (:folder!='Unread' OR unread=1))
                OR (:folder='Starred' AND flagged=1)
                OR (:folder='Sent' AND sent=1)
                OR (folder=:folder AND :folder NOT IN ('Inbox','Sent') AND (:account IS NOT NULL OR :folder IN ('Archive','Spam','Trash','Server Drafts')))
            ) AND (:query='' OR (account,id,staged) IN (SELECT account,id,staged FROM mail_search_fts WHERE mail_search_fts MATCH :query))
        )";
        let original_query = query;
        let query = search::expression(query)?;
        if !original_query.trim().is_empty() && query.is_empty() {
            return Ok(
                serde_json::json!({"messages":[],"total":0,"offset":offset,"page_size":100}),
            );
        }
        let total: usize = self.0.query_row(
            &format!("{sql} SELECT count(*) FROM filtered"),
            rusqlite::named_params! {":account":account,":folder":folder,":query":query},
            |r| r.get(0),
        )?;
        let mut statement = self.0.prepare(&format!("{sql} SELECT data FROM filtered ORDER BY timestamp DESC,account,id LIMIT 100 OFFSET :offset"))?;
        let mut messages = statement
            .query_map(
                rusqlite::named_params! {":account":account,":folder":folder,":query":query,":offset":offset as i64},
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str::<serde_json::Value>(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        for message in &mut messages {
            let account = message["account"].as_str().unwrap_or_default();
            let id = message["id"].as_str().unwrap_or_default();
            let pending = self.move_pending(account, id)?;
            message["pending_move"] = pending.into();
        }
        Ok(serde_json::json!({"messages":messages,"total":total,"offset":offset,"page_size":100}))
    }

    pub fn cached_message(&self, account: &str, id: &str) -> Result<Message> {
        self.cached_message_optional(account, id)?
            .context("Message is no longer cached")
    }
    pub fn cached_message_optional(&self, account: &str, id: &str) -> Result<Option<Message>> {
        use rusqlite::OptionalExtension;
        let data: Option<String> = self.0.query_row(
            "SELECT data FROM full_sync_messages WHERE account=?1 AND id=?2 UNION ALL SELECT data FROM messages WHERE account=?1 AND id=?2 LIMIT 1",
            params![account,id], |r| r.get(0)).optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }
    pub fn has_committed_messages(&self, account: &str) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE account=?1)",
            [account],
            |r| r.get(0),
        )?)
    }

    pub fn load(&self) -> Result<Vec<Message>> {
        let data = self
            .0
            .prepare("SELECT data FROM messages m WHERE NOT EXISTS (SELECT 1 FROM full_sync_messages f WHERE f.account=m.account AND f.id=m.id)
             UNION ALL SELECT data FROM full_sync_messages")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = data
            .iter()
            .map(|s| Ok(serde_json::from_str::<Message>(s)?))
            .collect::<Result<Vec<_>>>()?;
        messages.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        Ok(messages)
    }
    // A successful snapshot replaces only this account; network failure never deletes cache.
    pub fn snapshot(&mut self, account: &str, messages: &[Message], history: &str) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO accounts(email) VALUES (?1)",
            [account],
        )?;
        tx.execute("DELETE FROM messages WHERE account=?1", [account])?;
        for (position, message) in messages.iter().enumerate() {
            tx.execute(
                "INSERT INTO messages(account,id,position,data) VALUES (?1,?2,?3,?4)",
                params![
                    account,
                    message.id,
                    position,
                    serde_json::to_string(message)?
                ],
            )?;
        }
        tx.execute("DELETE FROM mail_pending_moves WHERE account=?1", [account])?;
        tx.execute("INSERT INTO sync_state VALUES (?1,unixepoch(),?2) ON CONFLICT(account) DO UPDATE SET last_sync=unixepoch(),history_id=excluded.history_id", params![account,history])?;
        tx.commit()?;
        Ok(())
    }
    pub fn full_sync(&self, account: &str) -> Result<Option<(String, String)>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT history,page FROM full_sync_state WHERE account=?1",
                [account],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn begin_full_sync(&mut self, account: &str, history: &str) -> Result<()> {
        let since = self.sync_history(account)?.cutoff();
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT OR IGNORE INTO accounts VALUES(?1)", [account])?;
        tx.execute(
            "INSERT INTO full_sync_state(account,history,page,since) VALUES(?1,?2,'',?3)",
            params![account, history, since],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn stage_message(&self, account: &str, message: &Message) -> Result<()> {
        self.0.execute("INSERT INTO full_sync_messages VALUES(?1,?2,?3) ON CONFLICT(account,id) DO UPDATE SET data=excluded.data", params![account,message.id,serde_json::to_string(message)?])?;
        Ok(())
    }
    pub fn staged_ids(&self, account: &str) -> Result<std::collections::BTreeSet<String>> {
        Ok(self
            .0
            .prepare("SELECT id FROM full_sync_messages WHERE account=?1")?
            .query_map([account], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn checkpoint_page(&self, account: &str, page: &str) -> Result<()> {
        self.0.execute(
            "UPDATE full_sync_state SET page=?2 WHERE account=?1",
            params![account, page],
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub fn finish_full_sync(&mut self, account: &str) -> Result<()> {
        self.finish_full_sync_cancellable(account, None)
    }
    pub fn history(&self, account: &str) -> Result<String> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT history_id FROM sync_state WHERE account=?1",
                [account],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }
    #[cfg(test)]
    pub fn draft_sender(&self) -> Result<String> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row("SELECT account FROM draft_sender WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_default())
    }
    #[cfg(test)]
    pub fn save_sender(&self, account: &str) -> Result<()> {
        self.0.execute("INSERT INTO draft_sender VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET account=excluded.account", [account])?;
        Ok(())
    }
    #[cfg(test)]
    pub fn draft_attachments(&self) -> Result<Vec<crate::attachments::DraftAttachment>> {
        Ok(self
            .0
            .prepare("SELECT name,data FROM draft_attachments ORDER BY position")?
            .query_map([], |r| {
                Ok(crate::attachments::DraftAttachment {
                    name: r.get(0)?,
                    data: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    #[cfg(test)]
    pub fn save_attachments(
        &mut self,
        files: &[crate::attachments::DraftAttachment],
    ) -> Result<()> {
        anyhow::ensure!(
            files.iter().map(|f| f.data.len()).sum::<usize>() <= crate::attachments::MAX_BYTES,
            "Attachments may total at most 20 MiB"
        );
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM draft_attachments", [])?;
        for (index, file) in files.iter().enumerate() {
            tx.execute(
                "INSERT INTO draft_attachments VALUES(?1,?2,?3)",
                params![index, file.name, file.data],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn cache_content(&mut self, message: &Message) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let html = serde_json::to_string(&message.html)?;
        let attachments = serde_json::to_string(&message.attachments)?;
        for table in ["messages", "full_sync_messages"] {
            tx.execute(&format!("UPDATE {table} SET data=json_set(data,'$.body',?3,'$.html',json(?4),'$.html_loaded',json(?5),'$.attachments',json(?6),'$.attachments_loaded',json('true'),'$.to_header',?7,'$.cc_header',?8,'$.reply_to',?9,'$.delivered_to',?10,'$.message_id',?11,'$.references',?12,'$.thread_id',?13,'$.recipients_loaded',json('true')) WHERE account=?1 AND id=?2"), params![message.account,message.id,message.body,html,serde_json::to_string(&message.html_loaded)?,attachments,message.to_header,message.cc_header,message.reply_to,message.delivered_to,message.message_id,message.references,message.thread_id])?;
        }
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn clear_draft(&mut self) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM drafts", [])?;
        tx.execute("DELETE FROM draft_attachments", [])?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn save_draft(&self, to: &str, subject: &str, body: &str) -> Result<()> {
        self.0.execute("INSERT INTO drafts VALUES(1,?1,?2,?3) ON CONFLICT(id) DO UPDATE SET recipient=excluded.recipient,subject=excluded.subject,body=excluded.body", params![to,subject,body])?;
        Ok(())
    }
    #[cfg(test)]
    pub fn draft(&self) -> Result<(String, String, String)> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT recipient,subject,body FROM drafts WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct CacheDir(PathBuf);
    impl CacheDir {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "focalmail-concurrency-{:032x}",
                rand::random::<u128>()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn path(&self) -> PathBuf {
            self.0.join("mail.sqlite3")
        }
    }
    impl Drop for CacheDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn account_folder_pages_isolate_accounts_custom_folders_and_search() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut first = crate::fixtures::messages()[0].clone();
        first.account = "one@example.com".into();
        first.folder = "Projects/Client: A".into();
        first.labels.clear();
        first.subject = "Needle".into();
        let mut second = first.clone();
        second.account = "two@example.com".into();
        s.snapshot(&first.account, &[first.clone()], "1").unwrap();
        s.snapshot(&second.account, &[second.clone()], "2").unwrap();
        s.replace_account_folders(
            &first.account,
            &["Empty folder".into(), first.folder.clone()],
        )
        .unwrap();
        let key = format!(
            "account-folder:{}",
            serde_json::json!([first.account, first.folder])
        );
        let page = s.mailbox_page(&key, "Needle", 0).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["messages"][0]["account"], first.account);
        assert_eq!(s.mailbox_page(&key, "absent", 0).unwrap()["total"], 0);
        let folders = s.account_folders().unwrap();
        assert!(folders[&first.account]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("Empty folder")));
        assert_eq!(s.history(&first.account).unwrap(), "1");
    }
    #[test]
    fn mailbox_dates_use_arrival_timestamps_not_sender_header_order() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut messages = crate::fixtures::messages()
            .into_iter()
            .take(3)
            .collect::<Vec<_>>();
        for (m, (id, timestamp, date)) in messages.iter_mut().zip([
            (
                "newest",
                1793943883000_i64,
                "Thu, 5 Nov 2026 21:44:43 -0800",
            ),
            ("middle", 1793939211000, "Thu, 05 Nov 2026 22:26:51 -0600"),
            ("older", 1793939128000, "Fri, 06 Nov 2026 09:55:28 +0530"),
        ]) {
            m.id = id.into();
            m.timestamp = timestamp;
            m.date = date.into();
            m.account = "me@example.com".into();
        }
        messages.reverse();
        store.snapshot("me@example.com", &messages, "1").unwrap();
        let page = store.mailbox_page("All Inboxes", "", 0).unwrap();
        let ids = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["newest", "middle", "older"]);
    }

    #[test]
    fn sync_counts_survive_restart_pause_and_retry_without_touching_mail() {
        let root = CacheDir::new();
        {
            let mut s = Store::at(root.path()).unwrap();
            s.begin_full_sync("me@example.com", "start").unwrap();
            s.set_sync_paused(true).unwrap();
            s.record_sync_started("me@example.com").unwrap();
            s.sync_progress("me@example.com", "importing", 42, Some(100))
                .unwrap();
            s.record_status(
                "me@example.com",
                &Err(crate::service::OperationCancelled.into()),
            )
            .unwrap();
        }
        let s = Store::at(root.path()).unwrap();
        assert!(s.sync_paused().unwrap());
        s.set_sync_paused(false).unwrap();
        let status = s.account_status().unwrap();
        assert_eq!(status[0]["state"], "idle");
        assert_eq!(status[0]["progress"]["phase"], "paused");
        assert_eq!(status[0]["progress"]["completed"], 42);
        s.record_sync_started("me@example.com").unwrap();
        s.sync_progress("me@example.com", "importing", 43, None)
            .unwrap();
        assert_eq!(s.account_status().unwrap()[0]["progress"]["total"], 100);
        s.record_status(
            "me@example.com",
            &Err(anyhow::anyhow!("Temporary provider failure")),
        )
        .unwrap();
        assert_eq!(s.account_status().unwrap()[0]["progress"]["phase"], "error");
        s.record_sync_started("me@example.com").unwrap();
        s.sync_progress("me@example.com", "scanning_folders", 0, None)
            .unwrap();
        assert!(s.account_status().unwrap()[0]["progress"]["total"].is_null());
        assert_eq!(s.account_status().unwrap()[0]["cached_count"], 0);
    }

    #[test]
    fn retry_replaces_the_old_failure_and_cancellation_clears_syncing() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        s.snapshot("me@example.com", &[], "1").unwrap();
        s.record_status(
            "me@example.com",
            &Err(anyhow::anyhow!("database is locked")),
        )
        .unwrap();
        s.record_sync_started("me@example.com").unwrap();
        assert_eq!(s.account_status().unwrap()[0]["state"], "syncing");
        assert_eq!(s.account_status().unwrap()[0]["detail"], "");
        s.record_status(
            "me@example.com",
            &Err(crate::service::OperationCancelled.into()),
        )
        .unwrap();
        assert_eq!(s.account_status().unwrap()[0]["state"], "idle");
        s.record_sync_started("me@example.com").unwrap();
        s.record_status("me@example.com", &Ok(())).unwrap();
        assert_eq!(s.account_status().unwrap()[0]["state"], "connected");
    }

    #[test]
    fn opening_a_ready_cache_reads_while_the_daemon_holds_a_writer() {
        let root = CacheDir::new();
        let mut writer = Store::at(root.path()).unwrap();
        let mut m = crate::fixtures::messages().remove(0);
        m.account = "me@example.com".into();
        writer.snapshot(&m.account, &[m.clone()], "1").unwrap();
        let tx = writer
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("INSERT INTO mail_preferences VALUES('pending',1)", [])
            .unwrap();
        let path = root.path();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = Store::at(path).and_then(|reader| {
                let page = reader.mailbox_page("All Inboxes", "", 0)?;
                Ok((
                    page["total"].as_u64().unwrap(),
                    reader.cached_message("me@example.com", "demo-0")?.subject,
                ))
            });
            send.send(result).unwrap();
        });
        // Roll back before asserting so a broken implementation can exit its wait.
        let result = receive.recv_timeout(Duration::from_secs(2));
        tx.rollback().unwrap();
        worker.join().unwrap();
        let (total, subject) = result
            .expect("Opening the cache tried to acquire a writer lock")
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(subject, m.subject);
    }

    #[test]
    fn sync_and_draft_commits_do_not_wait_for_a_reader_snapshot() {
        let root = CacheDir::new();
        let mut writer = Store::at(root.path()).unwrap();
        writer.begin_full_sync("me@example.com", "1").unwrap();
        let mut m = crate::fixtures::messages().remove(0);
        m.account = "me@example.com".into();
        writer.stage_message(&m.account, &m).unwrap();
        let reader = Store::at(root.path()).unwrap();
        reader
            .0
            .execute_batch("BEGIN; SELECT count(*) FROM full_sync_messages;")
            .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                m.id = "new-message".into();
                writer.stage_message(&m.account, &m)?;
                let mut draft = crate::drafts::Draft::new(m.account.clone());
                draft.body = "Locally saved while the mailbox is being read".into();
                writer.save_local_draft(&draft, Some(&[]))?;
                writer.finish_full_sync(&m.account)?;
                Ok(())
            })();
            send.send(result).unwrap();
        });
        let result = receive.recv_timeout(Duration::from_secs(2));
        reader.0.execute_batch("ROLLBACK").unwrap();
        worker.join().unwrap();
        result
            .expect("Reading cached mail blocked a sync or draft commit")
            .unwrap();
        assert_eq!(
            reader.mailbox_page("All Inboxes", "", 0).unwrap()["total"],
            2
        );
        assert_eq!(reader.list_drafts().unwrap().len(), 1);
        let journal: String = reader
            .0
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
    }

    #[test]
    fn concurrent_mailbox_migrations_preserve_cached_messages() {
        let root = CacheDir::new();
        {
            let mut s = Store::at(root.path()).unwrap();
            let mut m = crate::fixtures::messages().remove(0);
            m.account = "me@example.com".into();
            s.snapshot(&m.account, &[m.clone()], "1").unwrap();
            s.0.execute_batch(
                "DELETE FROM mailbox_summaries; DELETE FROM mail_migrations WHERE version=3;",
            )
            .unwrap();
        }
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers = (0..2)
            .map(|_| {
                let ready = ready.clone();
                let path = root.path();
                std::thread::spawn(move || {
                    ready.wait();
                    let s = Store::at(path)?;
                    Ok::<_, anyhow::Error>(
                        s.mailbox_page("All Inboxes", "", 0)?["total"]
                            .as_u64()
                            .unwrap(),
                    )
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), 1);
        }
    }

    #[test]
    fn mailbox_pages_search_full_bodies_and_load_one_complete_message() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let template = crate::fixtures::messages().remove(0);
        let messages = (0..215)
            .map(|i| {
                let mut m = template.clone();
                m.id = format!("message-{i:03}");
                m.account = "first@example.com".into();
                m.timestamp = i;
                m.labels = vec!["INBOX".into(), "SENT".into()];
                m.body = format!("{} hidden-search-{i}", "x".repeat(1000));
                m
            })
            .collect::<Vec<_>>();
        store.snapshot("first@example.com", &messages, "1").unwrap();
        let first = store.mailbox_page("All Inboxes", "", 0).unwrap();
        assert_eq!(first["total"], 215);
        assert_eq!(first["messages"].as_array().unwrap().len(), 100);
        assert_eq!(first["messages"][0]["id"], "message-214");
        assert_eq!(first["messages"][0]["body"].as_str().unwrap().len(), 160);
        let last = store.mailbox_page("first@example.com", "", 200).unwrap();
        assert_eq!(last["messages"].as_array().unwrap().len(), 15);
        let search = store.mailbox_page("Sent", "hidden-search-214", 0).unwrap();
        assert_eq!(search["total"], 1);
        assert!(
            store
                .cached_message("first@example.com", "message-214")
                .unwrap()
                .body
                .len()
                > 1000
        );
        assert!(store
            .cached_message("second@example.com", "message-214")
            .is_err());
        // Resumable downloads override old rows without duplicates or exposing Trash in Inbox.
        store.begin_full_sync("first@example.com", "2").unwrap();
        let mut changed = messages[214].clone();
        changed.folder = "Trash".into();
        changed.labels = vec!["TRASH".into(), "INBOX".into()];
        changed.body = "staged body".into();
        store.stage_message("first@example.com", &changed).unwrap();
        assert_eq!(
            store.mailbox_page("All Inboxes", "", 0).unwrap()["total"],
            214
        );
        assert_eq!(store.mailbox_page("Trash", "", 0).unwrap()["total"], 1);
        assert_eq!(
            store
                .cached_message("first@example.com", "message-214")
                .unwrap()
                .body,
            "staged body"
        );
        // Existing caches are indexed once, including partial initial syncs.
        store
            .0
            .execute_batch(
                "DELETE FROM mailbox_summaries; DELETE FROM mail_migrations WHERE version=3;",
            )
            .unwrap();
        store.migrate_mailbox().unwrap();
        store.migrate_mailbox().unwrap();
        assert_eq!(
            store.mailbox_page("All Inboxes", "", 0).unwrap()["total"],
            214
        );
        assert_eq!(store.mailbox_page("Trash", "", 0).unwrap()["total"], 1);
    }

    #[test]
    fn connection_state_survives_reopen_and_cancellation_preserves_it() {
        let root = std::env::temp_dir().join(format!(
            "focalmail-status-{:032x}.sqlite",
            rand::random::<u128>()
        ));
        {
            let mut store = Store::at(root.clone()).unwrap();
            store.snapshot("me@example.com", &[], "history").unwrap();
            store
                .record_status(
                    "me@example.com",
                    &Err(crate::service::ReconnectRequired.into()),
                )
                .unwrap();
        }
        let store = Store::at(root.clone()).unwrap();
        assert_eq!(
            store.account_status().unwrap()[0]["state"],
            "reconnect_required"
        );
        store
            .record_status(
                "me@example.com",
                &Err(crate::service::OperationCancelled.into()),
            )
            .unwrap();
        assert_eq!(
            store.account_status().unwrap()[0]["state"],
            "reconnect_required"
        );
        store.record_status("me@example.com", &Ok(())).unwrap();
        assert_eq!(store.account_status().unwrap()[0]["state"], "connected");
        assert_eq!(store.history("me@example.com").unwrap(), "history");
        drop(store);
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn multiple_drafts_and_files_survive_restart_and_delete_independently() {
        use crate::{attachments::DraftAttachment, drafts::Draft};
        let path =
            std::env::temp_dir().join(format!("focalmail-multi-{}.sqlite3", rand::random::<u64>()));
        let mut first = Draft::new("one@example.com".into());
        first.subject = "First".into();
        first.cc = "copy@example.com".into();
        let mut second = Draft::new("two@example.com".into());
        second.subject = "Second".into();
        {
            let mut store = Store::at(path.clone()).unwrap();
            store
                .save_local_draft(
                    &first,
                    Some(&[DraftAttachment {
                        name: "a.bin".into(),
                        data: vec![1, 2, 3],
                    }]),
                )
                .unwrap();
            store
                .save_local_draft(
                    &second,
                    Some(&[DraftAttachment {
                        name: "b.bin".into(),
                        data: vec![4, 5],
                    }]),
                )
                .unwrap();
            first.body = "Edited independently".into();
            store.save_local_draft(&first, None).unwrap();
        }
        {
            let store = Store::at(path.clone()).unwrap();
            assert_eq!(store.list_drafts().unwrap().len(), 2);
            assert_eq!(store.active_draft_id().unwrap(), Some(first.id.clone()));
            let (draft, files) = store.load_local_draft(&first.id).unwrap();
            assert_eq!(draft.body, "Edited independently");
            assert_eq!(draft.cc, "copy@example.com");
            assert_eq!(files[0].data, vec![1, 2, 3]);
            store.delete_local_draft(&first.id).unwrap();
            assert_eq!(store.list_drafts().unwrap().len(), 1);
            assert_eq!(
                store.load_local_draft(&second.id).unwrap().1[0].data,
                vec![4, 5]
            );
            let remaining: i64 = store
                .0
                .query_row(
                    "SELECT count(*) FROM local_draft_files WHERE draft_id=?1",
                    [&first.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(remaining, 0);
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn legacy_draft_migration_copies_sender_and_files_exactly_once() {
        let path = std::env::temp_dir().join(format!(
            "focalmail-migrate-draft-{}.sqlite3",
            rand::random::<u64>()
        ));
        {
            let mut store = Store::at(path.clone()).unwrap();
            store
                .save_draft("you@example.com", "Legacy", "Original body")
                .unwrap();
            store.save_sender("me@example.com").unwrap();
            store
                .save_attachments(&[crate::attachments::DraftAttachment {
                    name: "legacy.bin".into(),
                    data: vec![0, 255],
                }])
                .unwrap();
            store
                .0
                .execute("DELETE FROM mail_migrations WHERE version=2", [])
                .unwrap();
        }
        for _ in 0..2 {
            let store = Store::at(path.clone()).unwrap();
            assert_eq!(store.list_drafts().unwrap().len(), 1);
            let (draft, files) = store.load_local_draft("migrated-local-draft").unwrap();
            assert_eq!(draft.from, "me@example.com");
            assert_eq!(draft.to, "you@example.com");
            assert_eq!(draft.body, "Original body");
            assert_eq!(files[0].data, vec![0, 255]);
            assert!(store.draft().unwrap().0.is_empty());
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn failed_draft_file_write_rolls_back_text_and_attachments() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut draft = crate::drafts::Draft::new("me@example.com".into());
        draft.body = "Before".into();
        let file = crate::attachments::DraftAttachment {
            name: "original".into(),
            data: vec![42],
        };
        store.save_local_draft(&draft, Some(&[file])).unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_file BEFORE INSERT ON local_draft_files WHEN NEW.name='reject' BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        draft.body = "After".into();
        assert!(store
            .save_local_draft(
                &draft,
                Some(&[crate::attachments::DraftAttachment {
                    name: "reject".into(),
                    data: vec![99]
                }])
            )
            .is_err());
        let (saved, files) = store.load_local_draft(&draft.id).unwrap();
        assert_eq!(saved.body, "Before");
        assert_eq!(files[0].data, vec![42]);
    }

    #[test]
    fn attachment_metadata_updates_committed_and_partial_caches() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "me@example.com".into();
        store
            .snapshot(&message.account, std::slice::from_ref(&message), "10")
            .unwrap();
        store.begin_full_sync(&message.account, "11").unwrap();
        store.stage_message(&message.account, &message).unwrap();
        let files = [crate::attachments::Attachment {
            name: "old-mail.pdf".into(),
            size: 3,
            id: "part".into(),
            data: String::new(),
        }];
        message.attachments = files.to_vec();
        message.attachments_loaded = true;
        message.html = Some(crate::html::parse(
            "<h1>Formatted</h1><script>blocked()</script>",
        ));
        message.html_loaded = true;
        store.cache_content(&message).unwrap();
        let cached = store.load().unwrap();
        assert!(cached[0].attachments_loaded);
        assert!(cached[0].html_loaded);
        assert_eq!(cached[0].html.as_ref().unwrap().plain_text(), "Formatted");
        assert_eq!(cached[0].attachments[0].id, "part");
        store.finish_full_sync(&message.account).unwrap();
        assert_eq!(store.load().unwrap()[0].attachments[0].name, "old-mail.pdf");
    }

    #[test]
    fn draft_attachment_bytes_survive_restart_and_clear_with_draft() {
        use crate::attachments::DraftAttachment;
        let path = std::env::temp_dir().join(format!(
            "focalmail-draft-files-{}.sqlite3",
            rand::random::<u64>()
        ));
        {
            let mut store = Store::at(path.clone()).unwrap();
            store.save_draft("you@example.com", "Hi", "Body").unwrap();
            store
                .save_attachments(&[DraftAttachment {
                    name: "test.bin".into(),
                    data: vec![0, 255, 42],
                }])
                .unwrap();
        }
        {
            let mut store = Store::at(path.clone()).unwrap();
            assert_eq!(store.draft_attachments().unwrap()[0].data, vec![0, 255, 42]);
            store.clear_draft().unwrap();
            assert!(store.draft_attachments().unwrap().is_empty());
            assert_eq!(store.draft().unwrap(), Default::default());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn partial_sync_checkpoint_survives_restart_without_advancing_history() {
        let path = std::env::temp_dir().join(format!(
            "focalmail-partial-{}-{}.sqlite3",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let mut store = Store::at(path.clone()).unwrap();
            store.begin_full_sync("me@example.com", "start").unwrap();
            let mut message = crate::fixtures::messages().remove(0);
            message.account = "me@example.com".into();
            store.stage_message("me@example.com", &message).unwrap();
            store.checkpoint_page("me@example.com", "page2").unwrap();
        }
        {
            let mut store = Store::at(path.clone()).unwrap();
            assert_eq!(store.load().unwrap().len(), 1);
            assert_eq!(
                store.full_sync("me@example.com").unwrap(),
                Some(("start".into(), "page2".into()))
            );
            assert_eq!(store.history("me@example.com").unwrap(), "");
            store.finish_full_sync("me@example.com").unwrap();
            assert_eq!(store.load().unwrap().len(), 1);
            assert_eq!(store.history("me@example.com").unwrap(), "start");
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_inbox_cursor_is_invalidated_only_once_and_sender_survives_restart() {
        let path = std::env::temp_dir().join(format!(
            "focalmail-migration-{}-{}.sqlite3",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let mut store = Store::at(path.clone()).unwrap();
            store.snapshot("me@example.com", &[], "legacy").unwrap();
            store.0.execute("DELETE FROM mail_migrations", []).unwrap();
        }
        {
            let mut store = Store::at(path.clone()).unwrap();
            assert_eq!(store.history("me@example.com").unwrap(), "");
            store.snapshot("me@example.com", &[], "complete").unwrap();
            store.save_sender("me@example.com").unwrap();
        }
        {
            let store = Store::at(path.clone()).unwrap();
            assert_eq!(store.history("me@example.com").unwrap(), "complete");
            assert_eq!(store.draft_sender().unwrap(), "me@example.com");
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_server_profiles_are_not_connected_accounts() {
        use crate::settings::{Profile, Provider};
        let store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut profile = Profile {
            email: "me@example.com".into(),
            provider: Provider::Manual,
            host: "imap.example.com".into(),
            smtp_host: "smtp.example.com".into(),
            ..Default::default()
        };
        store.save_profile(&profile).unwrap();
        profile.smtp_port = 465;
        store.save_profile(&profile).unwrap();
        let saved = store.profiles().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].smtp_port, 465);
        assert!(store.accounts().unwrap().is_empty());
        profile.host.clear();
        assert!(store.save_profile(&profile).is_err());
        assert_eq!(store.profiles().unwrap()[0].host, "imap.example.com");
    }

    #[test]
    fn snapshot_replaces_only_target_account_and_drafts_roundtrip() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut messages = crate::fixtures::messages();
        messages[0].id = "a".into();
        store
            .snapshot("first@example.com", &messages[..1], "42")
            .unwrap();
        store
            .snapshot("second@example.com", &messages[..1], "43")
            .unwrap();
        store.snapshot("first@example.com", &[], "44").unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
        assert_eq!(store.accounts().unwrap().len(), 2);
        store
            .save_draft("a@example.com", "hello", "offline draft")
            .unwrap();
        assert_eq!(
            store.draft().unwrap(),
            (
                "a@example.com".into(),
                "hello".into(),
                "offline draft".into()
            )
        );
    }

    #[test]
    fn failed_snapshot_rolls_back_existing_cache() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut messages = crate::fixtures::messages();
        messages[0].id = "original".into();
        store
            .snapshot("me@example.com", &messages[..1], "10")
            .unwrap();
        messages[1].id = "original".into();
        assert!(store
            .snapshot("me@example.com", &messages[..2], "11")
            .is_err());
        let cached = store.load().unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].subject, messages[0].subject);
        let history: String = store
            .0
            .query_row("SELECT history_id FROM sync_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(history, "10");
    }

    #[test]
    fn file_cache_and_draft_survive_reopening() {
        let path = std::env::temp_dir().join(format!(
            "focalmail-cache-test-{}-{}.sqlite3",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let mut store = Store::at(path.clone()).unwrap();
            let messages = crate::fixtures::messages();
            store
                .snapshot("offline@example.com", &messages[..1], "20")
                .unwrap();
            store
                .save_draft("recipient@example.com", "Offline", "Saved body")
                .unwrap();
        }
        {
            let reopened = Store::at(path.clone()).unwrap();
            assert_eq!(reopened.accounts().unwrap(), vec!["offline@example.com"]);
            assert_eq!(reopened.load().unwrap()[0].subject, "Long time no see!");
            assert_eq!(reopened.draft().unwrap().2, "Saved body");
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    #[test]
    fn legacy_gmail_and_connected_provider_status_stay_distinct_from_saved_settings() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        store.snapshot("old@gmail.com", &[], "").unwrap();
        assert!(store.connected_profile("old@gmail.com").unwrap().is_none());
        let profile = crate::settings::Profile {
            email: "me@example.com".into(),
            provider: crate::settings::Provider::Microsoft365,
            client_id: "public-client".into(),
            ..Default::default()
        };
        store.save_profile(&profile).unwrap();
        assert_eq!(store.accounts().unwrap(), vec!["old@gmail.com"]);
        store.connect_profile(&profile).unwrap();
        let status = store.account_status().unwrap();
        assert_eq!(status[0]["provider"], "outlook");
        assert_eq!(status[1]["provider"], "gmail");
        assert!(store.notifications_enabled().unwrap());
        store.set_notifications(false).unwrap();
        assert!(!store.notifications_enabled().unwrap());
    }
    #[test]
    fn signature_upgrade_preserves_existing_mail_and_drafts() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let message = crate::fixtures::messages().remove(0);
        store
            .snapshot(&message.account, std::slice::from_ref(&message), "1")
            .unwrap();
        let draft = crate::drafts::Draft::new(message.account.clone());
        store.save_local_draft(&draft, Some(&[])).unwrap();
        // Model an existing version-nine cache, then reopen through normal migration.
        store
            .0
            .execute_batch(
                "DROP TABLE mail_signatures; DELETE FROM mail_migrations WHERE version=10;",
            )
            .unwrap();
        let store = Store::init(store.0).unwrap();
        assert_eq!(
            store
                .cached_message(&message.account, &message.id)
                .unwrap()
                .body,
            message.body
        );
        assert_eq!(
            store.load_local_draft(&draft.id).unwrap().0.from,
            draft.from
        );
        assert!(store.signature(&message.account).unwrap().html.is_empty());
        store
            .set_signature(
                &message.account,
                &crate::compose::Signature {
                    html: "<b>Saved signature</b>".into(),
                    new_messages: true,
                    replies_forwards: false,
                },
            )
            .unwrap();
        let store = Store::init(store.0).unwrap();
        assert_eq!(
            store.signature(&message.account).unwrap().html,
            "<b>Saved signature</b>"
        );
    }
}
