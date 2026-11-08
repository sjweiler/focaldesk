use crate::Message;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::PathBuf;

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
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
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
        Ok(store)
    }
    fn migrate_drafts(&mut self) -> Result<()> {
        let tx = self.0.transaction()?;
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
        let tx = self.0.transaction()?;
        tx.execute("INSERT INTO local_drafts VALUES(?1,?2,unixepoch()) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated=unixepoch()", params![draft.id,serde_json::to_string(draft)?])?;
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
    pub fn record_status(&self, email: &str, result: &Result<()>) -> Result<()> {
        let (state, detail) = match result {
            Ok(()) => ("connected", String::new()),
            Err(error) if error.is::<crate::service::OperationCancelled>() => return Ok(()),
            Err(error)
                if error
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.is_connect()) =>
            {
                (
                    "offline",
                    "Could not reach mail provider; cached mail remains available".into(),
                )
            }
            Err(error) if error.is::<crate::service::ReconnectRequired>() => {
                ("reconnect_required", error.to_string())
            }
            Err(error) => ("sync_error", error.to_string()),
        };
        self.0.execute("INSERT INTO account_status VALUES(?1,?2,?3,unixepoch()) ON CONFLICT(email) DO UPDATE SET state=excluded.state,detail=excluded.detail,updated=excluded.updated", params![email,state,detail])?;
        Ok(())
    }
    pub fn account_status(&self) -> Result<serde_json::Value> {
        let mut query = self.0.prepare("SELECT a.email,coalesce(s.state,'offline'),coalesce(s.detail,''),s.updated FROM accounts a LEFT JOIN account_status s ON a.email=s.email ORDER BY a.email")?;
        let accounts = query.query_map([], |r| Ok(serde_json::json!({"email":r.get::<_,String>(0)?, "provider":"gmail", "state":r.get::<_,String>(1)?, "detail":r.get::<_,String>(2)?, "updated":r.get::<_,Option<i64>>(3)?, "capabilities":["read","search","send","labels","attachments","reconnect"]})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut accounts = accounts;
        for account in &mut accounts {
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
        let tx = self.0.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO accounts VALUES(?1)",
            [&profile.email],
        )?;
        tx.execute("INSERT INTO connected_providers VALUES(?1,?2) ON CONFLICT(email) DO UPDATE SET profile=excluded.profile", params![profile.email,serde_json::to_string(profile)?])?;
        tx.commit()?;
        self.save_profile(profile)
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
        let tx = self.0.transaction()?;
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
        let tx = self.0.transaction()?;
        tx.execute("INSERT OR IGNORE INTO accounts VALUES(?1)", [account])?;
        tx.execute(
            "INSERT INTO full_sync_state VALUES(?1,?2,'')",
            params![account, history],
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
    pub fn finish_full_sync(&mut self, account: &str) -> Result<()> {
        let tx = self.0.transaction()?;
        let history: String = tx.query_row(
            "SELECT history FROM full_sync_state WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM messages WHERE account=?1", [account])?;
        tx.execute("INSERT INTO messages SELECT account,id,0,data FROM full_sync_messages WHERE account=?1", [account])?;
        tx.execute("INSERT INTO sync_state VALUES(?1,unixepoch(),?2) ON CONFLICT(account) DO UPDATE SET last_sync=unixepoch(),history_id=excluded.history_id", params![account,history])?;
        tx.execute("DELETE FROM full_sync_messages WHERE account=?1", [account])?;
        tx.execute("DELETE FROM full_sync_state WHERE account=?1", [account])?;
        tx.commit()?;
        Ok(())
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
        let tx = self.0.transaction()?;
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
        let tx = self.0.transaction()?;
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
        let tx = self.0.transaction()?;
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
        let mut message = crate::Mail::default().messages.remove(0);
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
            let mut message = crate::Mail::default().messages.remove(0);
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
        let mut messages = crate::Mail::default().messages;
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
        let mut messages = crate::Mail::default().messages;
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
            let messages = crate::Mail::default().messages;
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
}
