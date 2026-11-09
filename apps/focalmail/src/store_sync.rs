use super::*;
use crate::settings::SyncHistory;
use rusqlite::OptionalExtension;

impl Store {
    pub(super) fn migrate_sync_history(&mut self) -> Result<()> {
        {
            let tx = self
                .0
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=6)",
                [],
                |r| r.get::<_, bool>(0),
            )? {
                tx.commit()?;
                return Ok(());
            }
            let has_since: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('full_sync_state') WHERE name='since')", [], |r| r.get(0))?;
            if !has_since {
                tx.execute_batch("ALTER TABLE full_sync_state ADD COLUMN since INTEGER;")?;
            }
            tx.execute_batch("CREATE TABLE IF NOT EXISTS account_sync_preferences(account TEXT PRIMARY KEY REFERENCES accounts(email) ON DELETE CASCADE, history TEXT NOT NULL);
                UPDATE sync_state SET history_id='' WHERE account IN (SELECT account FROM full_sync_state);
                DELETE FROM full_sync_state;")?;
            tx.commit()?;
        }
        // FTS triggers hold pending index data until COMMIT. A single transaction
        // for a large initial import can consume many GiB of RAM. Move 32 rows at
        // a time, atomically: every message remains in one of the visible caches.
        // Leave version 6 unfinished until all rows move so a crash resumes safely.
        let mut preserved = 0;
        loop {
            let moved = self.preserve_partial_sync_batch(None)?;
            if moved == 0 {
                self.0
                    .execute("INSERT OR IGNORE INTO mail_migrations VALUES(6)", [])?;
                break;
            }
            preserved += moved;
            if preserved % 4096 == 0 {
                eprintln!("FocalMail: preserving imported cache ({preserved} messages)");
            }
        }
        Ok(())
    }

    fn preserve_partial_sync_batch(&mut self, account: Option<&str>) -> Result<usize> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = tx
            .prepare("SELECT account,id,data FROM full_sync_messages WHERE (?1 IS NULL OR account=?1) LIMIT 32")?
            .query_map([account], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (account, id, data) in &rows {
            tx.execute(
                "INSERT OR REPLACE INTO messages(account,id,position,data) VALUES(?1,?2,0,?3)",
                params![account, id, data],
            )?;
            tx.execute(
                "DELETE FROM full_sync_messages WHERE account=?1 AND id=?2",
                params![account, id],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    }

    pub fn sync_history(&self, account: &str) -> Result<SyncHistory> {
        let value: Option<String> = self
            .0
            .query_row(
                "SELECT history FROM account_sync_preferences WHERE account=?1",
                [account],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| Ok(serde_json::from_str(&v)?))
            .unwrap_or(Ok(SyncHistory::default()))
    }

    /// Called under the mail service's operation lock, after stopping background sync.
    pub fn set_sync_history(&mut self, account: &str, history: SyncHistory) -> Result<()> {
        anyhow::ensure!(
            self.accounts()?.iter().any(|a| a == account),
            "Account is no longer connected"
        );
        let previous = self.sync_history(account)?;
        if previous == history {
            return Ok(());
        }
        let shrinks = history != SyncHistory::All
            && (previous == SyncHistory::All || history.months() < previous.months());
        let keep_cursor =
            shrinks && self.full_sync(account)?.is_none() && !self.history(account)?.is_empty();
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO account_sync_preferences VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET history=excluded.history", params![account, serde_json::to_string(&history)?])?;
        // A pagination token/history cursor belongs to the old range. Keep committed
        // cache until a successful scan, so a failed expansion never loses mail.
        if !keep_cursor {
            tx.execute("UPDATE graph_folder_sync SET link='',initial=1,generation=generation+1 WHERE account=?1", [account])?;
            tx.execute(
                "UPDATE full_sync_state SET page='focalmail:abandoning' WHERE account=?1",
                [account],
            )?;
            tx.execute(
                "UPDATE sync_state SET history_id='' WHERE account=?1",
                [account],
            )?;
        }
        tx.commit()?;
        if !keep_cursor {
            self.abandon_full_sync(account)?;
        }
        self.prune_sync_history(account)
    }

    pub fn scan_since(&self, account: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .query_row(
                "SELECT since FROM full_sync_state WHERE account=?1",
                [account],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    fn abandon_full_sync(&mut self, account: &str) -> Result<()> {
        while self.preserve_partial_sync_batch(Some(account))? > 0 {}
        self.0
            .execute("DELETE FROM full_sync_state WHERE account=?1", [account])?;
        Ok(())
    }

    pub fn resume_abandoned_sync(&mut self, account: &str) -> Result<()> {
        if self
            .full_sync(account)?
            .is_some_and(|(_, page)| page == "focalmail:abandoning")
        {
            self.abandon_full_sync(account)?;
        }
        Ok(())
    }

    pub fn full_sync_finalizing(&self, account: &str) -> Result<bool> {
        Ok(self.full_sync(account)?.is_some_and(|(_, page)| {
            matches!(page.as_str(), "focalmail:clearing" | "focalmail:promoting")
        }))
    }

    /// History refreshes update only changed IDs. Rewriting the whole mailbox
    /// would load and reindex gigabytes even when no new messages arrived.
    pub fn apply_sync_changes(
        &mut self,
        account: &str,
        changes: &[(String, Option<Message>)],
        history: &str,
    ) -> Result<()> {
        for batch in changes.chunks(32) {
            let tx = self
                .0
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (id, message) in batch {
                if let Some(message) = message {
                    tx.execute("INSERT OR REPLACE INTO messages(account,id,position,data) VALUES(?1,?2,0,?3)", params![account,id,serde_json::to_string(message)?])?;
                } else {
                    tx.execute(
                        "DELETE FROM messages WHERE account=?1 AND id=?2",
                        params![account, id],
                    )?;
                }
            }
            tx.commit()?;
        }
        // Keep the old cursor until every batch commits. An interrupted refresh
        // replays the same idempotent updates instead of skipping messages.
        self.0.execute("INSERT INTO sync_state VALUES(?1,unixepoch(),?2) ON CONFLICT(account) DO UPDATE SET last_sync=unixepoch(),history_id=excluded.history_id", params![account,history])?;
        Ok(())
    }

    pub fn finish_full_sync_cancellable(
        &mut self,
        account: &str,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        loop {
            if let Some(cancel) = cancel {
                crate::providers::cancelled(cancel)?;
            }
            if self.finish_full_sync_step(account)? {
                return Ok(());
            }
        }
    }

    /// A durable phase marker distinguishes clearing the old snapshot from
    /// promoting the complete replacement. Neither phase requires all FTS writes
    /// in memory, and a restart must never clear already-promoted messages again.
    fn finish_full_sync_step(&mut self, account: &str) -> Result<bool> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (history, phase): (String, String) = tx.query_row(
            "SELECT history,page FROM full_sync_state WHERE account=?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut finished = false;
        match phase.as_str() {
            "focalmail:clearing" => {
                let removed = tx.execute("DELETE FROM messages WHERE rowid IN (SELECT rowid FROM messages WHERE account=?1 LIMIT 32)", [account])?;
                if removed == 0 {
                    tx.execute(
                        "UPDATE full_sync_state SET page='focalmail:promoting' WHERE account=?1",
                        [account],
                    )?;
                }
            }
            "focalmail:promoting" => {
                let rows = tx
                    .prepare("SELECT id,data FROM full_sync_messages WHERE account=?1 LIMIT 32")?
                    .query_map([account], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (id, data) in &rows {
                    tx.execute("INSERT OR REPLACE INTO messages(account,id,position,data) VALUES(?1,?2,0,?3)", params![account,id,data])?;
                    tx.execute(
                        "DELETE FROM full_sync_messages WHERE account=?1 AND id=?2",
                        params![account, id],
                    )?;
                }
                if rows.is_empty() {
                    tx.execute("DELETE FROM mail_pending_moves WHERE account=?1", [account])?;
                    tx.execute("INSERT INTO sync_state VALUES(?1,unixepoch(),?2) ON CONFLICT(account) DO UPDATE SET last_sync=unixepoch(),history_id=excluded.history_id", params![account,history])?;
                    tx.execute("DELETE FROM full_sync_state WHERE account=?1", [account])?;
                    finished = true;
                }
            }
            _ => {
                tx.execute(
                    "UPDATE full_sync_state SET page='focalmail:clearing' WHERE account=?1",
                    [account],
                )?;
            }
        }
        tx.commit()?;
        Ok(finished)
    }

    /// Only local cache rows are removed; no provider mutation is issued.
    pub fn prune_sync_history(&self, account: &str) -> Result<()> {
        if let Some(since) = self.sync_history(account)?.cutoff() {
            while self.0.execute("DELETE FROM messages WHERE rowid IN (SELECT m.rowid FROM mailbox_summaries s JOIN messages m ON m.account=s.account AND m.id=s.id WHERE s.account=?1 AND s.staged=0 AND s.timestamp>0 AND s.timestamp<?2 AND NOT EXISTS(SELECT 1 FROM mail_pending_moves p WHERE p.account=m.account AND p.id=m.id) LIMIT 32)", params![account, since])? > 0 {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_and_narrower_history_preserve_a_completed_sync_cursor() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        s.snapshot("me@example.com", &[], "valid-history").unwrap();
        s.0.execute("DELETE FROM mail_migrations WHERE version=6", [])
            .unwrap();
        let mut s = Store::init(s.0).unwrap();
        assert_eq!(s.history("me@example.com").unwrap(), "valid-history");
        s.set_sync_history("me@example.com", SyncHistory::OneMonth)
            .unwrap();
        assert_eq!(s.history("me@example.com").unwrap(), "valid-history");
        assert!(s.full_sync("me@example.com").unwrap().is_none());
        s.set_sync_history("me@example.com", SyncHistory::All)
            .unwrap();
        assert_eq!(s.history("me@example.com").unwrap(), "");
    }

    #[test]
    fn finalization_survives_restarts_in_both_phases_and_cancellation_keeps_the_cursor() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "me@example.com".into();
        let old = (0..65)
            .map(|i| {
                let mut m = message.clone();
                m.id = format!("old-{i}");
                m
            })
            .collect::<Vec<_>>();
        s.snapshot(&message.account, &old, "old-history").unwrap();
        s.begin_full_sync(&message.account, "new-history").unwrap();
        for i in 0..65 {
            message.id = format!("new-{i}");
            s.stage_message(&message.account, &message).unwrap();
        }
        assert!(!s.finish_full_sync_step(&message.account).unwrap());
        assert!(!s.finish_full_sync_step(&message.account).unwrap());
        let mut s = Store::init(s.0).unwrap();
        assert!(s.full_sync_finalizing(&message.account).unwrap());
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(s
            .finish_full_sync_cancellable(&message.account, Some(&cancelled))
            .unwrap_err()
            .is::<crate::service::OperationCancelled>());
        assert_eq!(s.history(&message.account).unwrap(), "old-history");
        while s.full_sync(&message.account).unwrap().unwrap().1 != "focalmail:promoting" {
            s.finish_full_sync_step(&message.account).unwrap();
        }
        assert!(!s.finish_full_sync_step(&message.account).unwrap());
        let mut s = Store::init(s.0).unwrap();
        assert_eq!(s.cached_ids(&message.account).unwrap().len(), 65);
        s.finish_full_sync(&message.account).unwrap();
        assert_eq!(s.cached_ids(&message.account).unwrap().len(), 65);
        assert!(s
            .cached_ids(&message.account)
            .unwrap()
            .iter()
            .all(|id| id.starts_with("new-")));
        assert_eq!(s.history(&message.account).unwrap(), "new-history");
        assert!(s.full_sync(&message.account).unwrap().is_none());
    }

    #[test]
    fn history_refresh_never_rewrites_unchanged_messages_and_advances_after_changes_commit() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "me@example.com".into();
        s.snapshot(&message.account, &[message.clone()], "10")
            .unwrap();
        // Any attempt to recreate the unchanged cache row is a regression.
        s.0.execute_batch("CREATE TRIGGER reject_rewrite BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT,'unchanged cache rewritten'); END;").unwrap();
        s.apply_sync_changes(&message.account, &[], "11").unwrap();
        assert_eq!(s.history(&message.account).unwrap(), "11");
        assert!(s
            .apply_sync_changes(
                &message.account,
                &[(message.id.clone(), Some(message.clone()))],
                "12"
            )
            .is_err());
        assert_eq!(s.history(&message.account).unwrap(), "11");
        s.apply_sync_changes(&message.account, &[(message.id.clone(), None)], "13")
            .unwrap();
        assert!(s.cached_ids(&message.account).unwrap().is_empty());
        assert_eq!(s.history(&message.account).unwrap(), "13");
    }

    #[test]
    fn interrupted_upgrade_resumes_bounded_batches_without_losing_partial_mail() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "me@example.com".into();
        s.begin_full_sync(&message.account, "old-scan").unwrap();
        for i in 0..65 {
            message.id = format!("message-{i:03}");
            s.stage_message(&message.account, &message).unwrap();
        }
        s.0.execute("DELETE FROM mail_migrations WHERE version=6", [])
            .unwrap();
        assert_eq!(s.preserve_partial_sync_batch(None).unwrap(), 32);
        assert_eq!(s.staged_ids(&message.account).unwrap().len(), 33);
        assert_eq!(s.cached_ids(&message.account).unwrap().len(), 65);
        let s = Store::init(s.0).unwrap();
        assert_eq!(s.cached_ids(&message.account).unwrap().len(), 65);
        assert!(s.staged_ids(&message.account).unwrap().is_empty());
        assert!(s.full_sync(&message.account).unwrap().is_none());
        assert_eq!(
            s.0.query_row(
                "SELECT count(*) FROM mail_migrations WHERE version=6",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn upgrade_preserves_partial_mail_and_drafts_but_restarts_unbounded_scans_once() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "me@example.com".into();
        s.snapshot(&message.account, &[], "old-history").unwrap();
        s.begin_full_sync(&message.account, "old-scan").unwrap();
        s.stage_message(&message.account, &message).unwrap();
        let draft = crate::drafts::Draft::new(message.account.clone());
        s.save_local_draft(&draft, None).unwrap();
        // Recreate the prior schema to exercise the actual upgrade, not a fresh DB.
        s.0.execute_batch("DROP TABLE account_sync_preferences; ALTER TABLE full_sync_state DROP COLUMN since; DELETE FROM mail_migrations WHERE version=6;").unwrap();
        let mut s = Store::init(s.0).unwrap();
        assert_eq!(
            s.cached_message(&message.account, &message.id).unwrap().id,
            message.id
        );
        assert!(s.full_sync(&message.account).unwrap().is_none());
        assert_eq!(s.history(&message.account).unwrap(), "");
        assert_eq!(s.load_local_draft(&draft.id).unwrap().0.id, draft.id);
        assert_eq!(
            s.sync_history(&message.account).unwrap(),
            SyncHistory::OneYear
        );
        s.snapshot(&message.account, &[message.clone()], "new-history")
            .unwrap();
        let s = Store::init(s.0).unwrap();
        assert_eq!(s.history(&message.account).unwrap(), "new-history");
    }

    #[test]
    fn shrinking_history_prunes_only_this_accounts_cache_and_invalidates_checkpoints() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut recent = crate::fixtures::messages().remove(0);
        recent.account = "me@example.com".into();
        recent.id = "recent".into();
        recent.timestamp = chrono::Utc::now().timestamp_millis();
        let mut old = recent.clone();
        old.id = "old".into();
        old.timestamp = (chrono::Utc::now() - chrono::Duration::days(100)).timestamp_millis();
        s.snapshot(&recent.account, &[recent.clone(), old.clone()], "10")
            .unwrap();
        s.snapshot("other@example.com", &[old.clone()], "20")
            .unwrap();
        s.begin_full_sync(&recent.account, "11").unwrap();
        s.stage_message(&recent.account, &old).unwrap();
        s.checkpoint_page(&recent.account, "page").unwrap();
        s.set_sync_history(&recent.account, SyncHistory::OneMonth)
            .unwrap();
        assert_eq!(
            s.sync_history(&recent.account).unwrap(),
            SyncHistory::OneMonth
        );
        assert_eq!(
            s.sync_history("other@example.com").unwrap(),
            SyncHistory::OneYear
        );
        assert_eq!(s.history(&recent.account).unwrap(), "");
        assert_eq!(s.history("other@example.com").unwrap(), "20");
        assert!(s.full_sync(&recent.account).unwrap().is_none());
        assert!(s.staged_ids(&recent.account).unwrap().is_empty());
        assert_eq!(s.cached_ids(&recent.account).unwrap().len(), 1);
        assert_eq!(s.cached_ids("other@example.com").unwrap().len(), 1);
        assert!(s
            .set_sync_history("missing@example.com", SyncHistory::All)
            .is_err());
        s.set_sync_history(&recent.account, SyncHistory::All)
            .unwrap();
        assert_eq!(s.cached_ids(&recent.account).unwrap().len(), 1);
        s.begin_full_sync(&recent.account, "12").unwrap();
        assert!(s.scan_since(&recent.account).unwrap().is_none());
        s.remove_account(&recent.account, || Ok(())).unwrap();
        assert_eq!(
            s.0.query_row("SELECT count(*) FROM account_sync_preferences", [], |r| {
                r.get::<_, usize>(0)
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn background_skips_revoked_accounts_and_restart_preserves_pause_and_checkpoints() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        for account in [
            "expired@example.com",
            "offline@example.com",
            "working@example.com",
        ] {
            s.snapshot(account, &[], "10").unwrap();
        }
        s.record_status(
            "expired@example.com",
            &Err(crate::service::ReconnectRequired.into()),
        )
        .unwrap();
        s.record_status(
            "offline@example.com",
            &Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "test").into()),
        )
        .unwrap();
        s.begin_full_sync("working@example.com", "11").unwrap();
        s.checkpoint_page("working@example.com", "page-two")
            .unwrap();
        s.record_sync_started("working@example.com").unwrap();
        s.sync_progress("working@example.com", "importing", 123, None)
            .unwrap();
        s.set_sync_paused(true).unwrap();
        s.recover_interrupted_sync().unwrap();
        assert!(s.sync_paused().unwrap());
        assert_eq!(
            s.background_accounts().unwrap(),
            ["offline@example.com", "working@example.com"]
        );
        assert_eq!(
            s.full_sync("working@example.com").unwrap().unwrap().1,
            "page-two"
        );
        let statuses = s.account_status().unwrap();
        assert_eq!(statuses[0]["state"], "reconnect_required");
        assert_eq!(statuses[1]["state"], "offline");
        assert_eq!(statuses[2]["state"], "idle");
        assert_eq!(statuses[2]["progress"]["completed"], 123);
        s.record_status("expired@example.com", &Ok(())).unwrap();
        assert_eq!(s.background_accounts().unwrap().len(), 3);
    }

    #[test]
    fn preferences_survive_reopen_and_same_range_keeps_resumable_import() {
        let path =
            std::env::temp_dir().join(format!("focalmail-sync-{}.sqlite3", rand::random::<u64>()));
        {
            let mut s = Store::at(path.clone()).unwrap();
            s.snapshot("me@example.com", &[], "10").unwrap();
            s.set_sync_history("me@example.com", SyncHistory::SixMonths)
                .unwrap();
            s.begin_full_sync("me@example.com", "11").unwrap();
            s.checkpoint_page("me@example.com", "page-two").unwrap();
            s.set_sync_history("me@example.com", SyncHistory::SixMonths)
                .unwrap();
        }
        {
            let s = Store::at(path.clone()).unwrap();
            assert_eq!(
                s.sync_history("me@example.com").unwrap(),
                SyncHistory::SixMonths
            );
            assert_eq!(
                s.full_sync("me@example.com").unwrap().unwrap().1,
                "page-two"
            );
            assert!(s.scan_since("me@example.com").unwrap().is_some());
        }
        std::fs::remove_file(path).unwrap();
    }
}
