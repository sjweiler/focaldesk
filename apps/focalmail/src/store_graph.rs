//! Microsoft Graph checkpoints and message changes. Pages become visible before
//! their next/delta link advances; interrupted pages can be replayed safely.
use super::*;

pub struct GraphFolderState {
    pub link: String,
    pub initial: bool,
}

impl Store {
    pub(super) fn migrate_graph(&self) -> Result<()> {
        self.0.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS graph_folder_sync(
                account TEXT NOT NULL REFERENCES accounts(email) ON DELETE CASCADE,
                folder_id TEXT NOT NULL,path TEXT NOT NULL,link TEXT NOT NULL DEFAULT '',
                initial INTEGER NOT NULL DEFAULT 1,generation INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY(account,folder_id));
            CREATE TABLE IF NOT EXISTS graph_membership(
                account TEXT NOT NULL,id TEXT NOT NULL,folder_id TEXT NOT NULL,
                generation INTEGER NOT NULL,change_key TEXT NOT NULL DEFAULT '',PRIMARY KEY(account,id),
                FOREIGN KEY(account,folder_id) REFERENCES graph_folder_sync(account,folder_id) ON DELETE CASCADE);
            CREATE INDEX IF NOT EXISTS graph_membership_folder ON graph_membership(account,folder_id,generation);
            INSERT OR IGNORE INTO mail_migrations VALUES(8);
            UPDATE graph_folder_sync SET link='',initial=1,generation=generation+1
                WHERE NOT EXISTS(SELECT 1 FROM mail_migrations WHERE version=9);
            INSERT OR IGNORE INTO mail_migrations VALUES(9);
            COMMIT;")?;
        Ok(())
    }
    pub fn graph_prepare_folder(
        &mut self,
        account: &str,
        folder_id: &str,
        path: &str,
    ) -> Result<GraphFolderState> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO graph_folder_sync(account,folder_id,path) VALUES(?1,?2,?3)",
            params![account, folder_id, path],
        )?;
        // Adopt legacy cached rows without dropping them during the first delta import.
        tx.execute("INSERT OR IGNORE INTO graph_membership(account,id,folder_id,generation)
            SELECT account,id,?2,0 FROM mailbox_summaries WHERE account=?1 AND folder=?3 AND staged=0",params![account,folder_id,path])?;
        let old_path: String = tx.query_row(
            "SELECT path FROM graph_folder_sync WHERE account=?1 AND folder_id=?2",
            params![account, folder_id],
            |r| r.get(0),
        )?;
        if old_path != path {
            tx.execute(
                "UPDATE messages SET data=json_set(data,'$.folder',?3) WHERE account=?1 AND id IN
                (SELECT id FROM graph_membership WHERE account=?1 AND folder_id=?2)",
                params![account, folder_id, path],
            )?;
            tx.execute(
                "UPDATE graph_folder_sync SET path=?3 WHERE account=?1 AND folder_id=?2",
                params![account, folder_id, path],
            )?;
        }
        let state = tx.query_row(
            "SELECT link,initial FROM graph_folder_sync WHERE account=?1 AND folder_id=?2",
            params![account, folder_id],
            |r| {
                Ok(GraphFolderState {
                    link: r.get(0)?,
                    initial: r.get(1)?,
                })
            },
        )?;
        tx.commit()?;
        Ok(state)
    }
    pub fn graph_change_key(&self, account: &str, id: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT change_key FROM graph_membership WHERE account=?1 AND id=?2",
                params![account, id],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn graph_reset_folder(&self, account: &str, folder_id: &str) -> Result<()> {
        self.0.execute("UPDATE graph_folder_sync SET link='',initial=1,generation=generation+1 WHERE account=?1 AND folder_id=?2",params![account,folder_id])?;
        Ok(())
    }
    pub fn graph_apply_page(
        &mut self,
        account: &str,
        folder_id: &str,
        changes: &[(String, Option<Message>, String)],
        link: &str,
        complete: bool,
    ) -> Result<()> {
        // Bound FTS pending data while keeping the previous link until all rows commit.
        for batch in changes.chunks(32) {
            let tx = self
                .0
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (id, message, change_key) in batch {
                if let Some(message) = message {
                    tx.execute("INSERT OR REPLACE INTO messages(account,id,position,data) SELECT ?1,?2,0,?3
                        WHERE NOT EXISTS(SELECT 1 FROM messages WHERE account=?1 AND id=?2 AND data=?3)",params![account,id,serde_json::to_string(message)?])?;
                    tx.execute("INSERT INTO graph_membership(account,id,folder_id,generation,change_key)
                        SELECT ?1,?2,?3,generation,?4 FROM graph_folder_sync WHERE account=?1 AND folder_id=?3
                        ON CONFLICT(account,id) DO UPDATE SET folder_id=excluded.folder_id,generation=excluded.generation,change_key=excluded.change_key",params![account,id,folder_id,change_key])?;
                    tx.execute(
                        "DELETE FROM mail_pending_moves WHERE account=?1 AND id=?2",
                        params![account, id],
                    )?;
                } else {
                    // A move can arrive in the destination before the source removal.
                    // Only remove a message if this folder still owns it.
                    let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM graph_membership WHERE account=?1 AND id=?2 AND folder_id=?3)",params![account,id,folder_id],|r|r.get(0))?;
                    if owned {
                        tx.execute(
                            "DELETE FROM messages WHERE account=?1 AND id=?2",
                            params![account, id],
                        )?;
                        tx.execute(
                            "DELETE FROM graph_membership WHERE account=?1 AND id=?2",
                            params![account, id],
                        )?;
                        tx.execute(
                            "DELETE FROM mail_pending_moves WHERE account=?1 AND id=?2",
                            params![account, id],
                        )?;
                    }
                }
            }
            tx.commit()?;
        }
        if complete {
            // Expired cursors reconcile only their own folder after a complete scan.
            while self.0.execute("DELETE FROM messages WHERE account=?1 AND id IN (SELECT m.id FROM graph_membership m
                JOIN graph_folder_sync f ON f.account=m.account AND f.folder_id=m.folder_id
                WHERE m.account=?1 AND m.folder_id=?2 AND f.initial=1 AND m.generation!=f.generation AND EXISTS(SELECT 1 FROM messages v WHERE v.account=m.account AND v.id=m.id) LIMIT 32)",params![account,folder_id])? > 0 {}
            self.0.execute("DELETE FROM graph_membership WHERE account=?1 AND folder_id=?2 AND generation!=(SELECT generation FROM graph_folder_sync WHERE account=?1 AND folder_id=?2) AND (SELECT initial FROM graph_folder_sync WHERE account=?1 AND folder_id=?2)=1",params![account,folder_id])?;
        }
        self.0.execute("UPDATE graph_folder_sync SET link=?3,initial=CASE WHEN ?4 THEN 0 ELSE initial END WHERE account=?1 AND folder_id=?2",params![account,folder_id,link,complete])?;
        Ok(())
    }
    pub fn graph_finish_account(&mut self, account: &str, folder_ids: &[String]) -> Result<()> {
        let removed = self
            .0
            .prepare("SELECT folder_id FROM graph_folder_sync WHERE account=?1")?
            .query_map([account], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in removed.iter().filter(|id| !folder_ids.contains(id)) {
            while self.0.execute("DELETE FROM messages WHERE account=?1 AND id IN (SELECT g.id FROM graph_membership g JOIN messages v ON v.account=g.account AND v.id=g.id WHERE g.account=?1 AND g.folder_id=?2 LIMIT 32)",params![account,id])? > 0 {}
            self.0.execute(
                "DELETE FROM graph_folder_sync WHERE account=?1 AND folder_id=?2",
                params![account, id],
            )?;
        }
        self.0.execute("DELETE FROM mail_pending_moves WHERE account=?1 AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.account=?1 AND m.id=mail_pending_moves.id)", [account])?;
        self.0.execute("INSERT INTO sync_state VALUES(?1,unixepoch(),'graph-delta-v1') ON CONFLICT(account) DO UPDATE SET last_sync=unixepoch(),history_id=excluded.history_id",[account])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(account: &str, id: &str, folder: &str) -> Message {
        let mut message = crate::fixtures::messages()[0].clone();
        message.account = account.into();
        message.id = id.into();
        message.folder = folder.into();
        message.labels.clear();
        message
    }
    #[test]
    fn folder_moves_are_safe_in_both_orders_and_rename_preserves_hydrated_content() {
        for destination_first in [false, true] {
            let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
            let account = "me@example.com";
            let mut original = message(account, "id", "Inbox");
            original.body = "Offline body".into();
            original.attachments_loaded = true;
            s.snapshot(account, &[original.clone()], "old").unwrap();
            s.graph_prepare_folder(account, "inbox", "Inbox").unwrap();
            s.graph_apply_page(
                account,
                "inbox",
                &[("id".into(), Some(original.clone()), "key".into())],
                "inbox-delta",
                true,
            )
            .unwrap();
            s.graph_prepare_folder(account, "projects", "Projects")
                .unwrap();
            let mut moved = original.clone();
            moved.folder = "Projects".into();
            if !destination_first {
                s.graph_apply_page(
                    account,
                    "inbox",
                    &[("id".into(), None, String::new())],
                    "next",
                    true,
                )
                .unwrap();
            }
            s.graph_apply_page(
                account,
                "projects",
                &[("id".into(), Some(moved), "key".into())],
                "projects-delta",
                true,
            )
            .unwrap();
            if destination_first {
                s.graph_apply_page(
                    account,
                    "inbox",
                    &[("id".into(), None, String::new())],
                    "next",
                    true,
                )
                .unwrap();
            }
            s.graph_prepare_folder(account, "projects", "Renamed/Projects")
                .unwrap();
            let cached = s.cached_message(account, "id").unwrap();
            assert_eq!(cached.folder, "Renamed/Projects");
            assert_eq!(cached.body, original.body);
            assert!(cached.attachments_loaded);
            assert_eq!(s.cached_ids(account).unwrap().len(), 1);
        }
    }
    #[test]
    fn cursor_reset_cleans_more_than_one_batch_only_when_the_folder_finishes() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let account = "me@example.com";
        let messages = (0..70)
            .map(|i| message(account, &format!("id{i}"), "Inbox"))
            .collect::<Vec<_>>();
        s.snapshot(account, &messages, "old").unwrap();
        s.graph_prepare_folder(account, "inbox", "Inbox").unwrap();
        s.graph_apply_page(
            account,
            "inbox",
            &messages
                .iter()
                .map(|m| (m.id.clone(), Some(m.clone()), "v1".into()))
                .collect::<Vec<_>>(),
            "old-delta",
            true,
        )
        .unwrap();
        s.graph_reset_folder(account, "inbox").unwrap();
        s.graph_apply_page(
            account,
            "inbox",
            &[(
                messages[0].id.clone(),
                Some(messages[0].clone()),
                "v1".into(),
            )],
            "page-two",
            false,
        )
        .unwrap();
        assert_eq!(s.cached_ids(account).unwrap().len(), 70);
        assert!(
            s.graph_prepare_folder(account, "inbox", "Inbox")
                .unwrap()
                .initial
        );
        s.graph_apply_page(account, "inbox", &[], "new-delta", true)
            .unwrap();
        assert_eq!(s.cached_ids(account).unwrap().len(), 1);
        assert!(
            !s.graph_prepare_folder(account, "inbox", "Inbox")
                .unwrap()
                .initial
        );
        assert_eq!(
            s.graph_prepare_folder(account, "inbox", "Inbox")
                .unwrap()
                .link,
            "new-delta"
        );
    }
    #[test]
    fn page_failure_keeps_cursor_and_failed_batch_rolls_back() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let account = "me@example.com";
        s.snapshot(account, &[], "old").unwrap();
        s.graph_prepare_folder(account, "inbox", "Inbox").unwrap();
        s.graph_apply_page(account, "inbox", &[], "page-one", false)
            .unwrap();
        s.0.execute_batch("CREATE TRIGGER fail_graph BEFORE INSERT ON messages WHEN NEW.id='fail' BEGIN SELECT RAISE(ABORT,'disk failure'); END;").unwrap();
        let changes = vec![
            (
                "good".into(),
                Some(message(account, "good", "Inbox")),
                "key".into(),
            ),
            (
                "fail".into(),
                Some(message(account, "fail", "Inbox")),
                "key".into(),
            ),
        ];
        assert!(s
            .graph_apply_page(account, "inbox", &changes, "page-two", false)
            .is_err());
        assert!(s.cached_ids(account).unwrap().is_empty());
        assert_eq!(
            s.graph_prepare_folder(account, "inbox", "Inbox")
                .unwrap()
                .link,
            "page-one"
        );
    }
    #[test]
    fn history_expansion_resets_delta_cursors_and_folder_removal_is_account_scoped() {
        use crate::settings::SyncHistory;
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        for account in ["one@example.com", "two@example.com"] {
            let m = message(account, "same-id", "Inbox");
            s.snapshot(account, &[m.clone()], "graph-delta-v1").unwrap();
            s.graph_prepare_folder(account, "inbox", "Inbox").unwrap();
            s.graph_apply_page(
                account,
                "inbox",
                &[(m.id.clone(), Some(m), "key".into())],
                "saved-delta",
                true,
            )
            .unwrap();
        }
        s.set_sync_history("one@example.com", SyncHistory::All)
            .unwrap();
        assert!(s
            .graph_prepare_folder("one@example.com", "inbox", "Inbox")
            .unwrap()
            .link
            .is_empty());
        assert_eq!(
            s.graph_prepare_folder("two@example.com", "inbox", "Inbox")
                .unwrap()
                .link,
            "saved-delta"
        );
        s.graph_finish_account("one@example.com", &[]).unwrap();
        assert!(s.cached_ids("one@example.com").unwrap().is_empty());
        assert_eq!(s.cached_ids("two@example.com").unwrap().len(), 1);
    }
    #[test]
    fn partial_page_replays_after_restart_without_advancing_or_losing_committed_rows() {
        let path = std::env::temp_dir().join(format!(
            "focalmail-graph-replay-{}.sqlite3",
            rand::random::<u64>()
        ));
        let account = "me@example.com";
        let changes = (0..33)
            .map(|i| {
                let id = format!("id{i}");
                (
                    id.clone(),
                    Some(message(account, &id, "Inbox")),
                    "key".into(),
                )
            })
            .collect::<Vec<_>>();
        {
            let mut s = Store::at(path.clone()).unwrap();
            s.snapshot(account, &[], "old-history").unwrap();
            s.graph_prepare_folder(account, "inbox", "Inbox").unwrap();
            s.graph_apply_page(account, "inbox", &[], "saved-page", false)
                .unwrap();
            s.0.execute_batch("CREATE TRIGGER fail_graph BEFORE INSERT ON messages WHEN NEW.id='id32' BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
            assert!(s
                .graph_apply_page(account, "inbox", &changes, "delta", true)
                .is_err());
            assert_eq!(s.cached_ids(account).unwrap().len(), 32);
        }
        {
            let mut s = Store::at(path.clone()).unwrap();
            assert_eq!(
                s.graph_prepare_folder(account, "inbox", "Inbox")
                    .unwrap()
                    .link,
                "saved-page"
            );
            s.0.execute_batch("DROP TRIGGER fail_graph").unwrap();
            s.graph_apply_page(account, "inbox", &changes, "delta", true)
                .unwrap();
            assert_eq!(s.cached_ids(account).unwrap().len(), 33);
            assert_eq!(
                s.graph_prepare_folder(account, "inbox", "Inbox")
                    .unwrap()
                    .link,
                "delta"
            );
            assert_eq!(s.history(account).unwrap(), "old-history");
        }
        let _ = std::fs::remove_file(path);
    }
}
