//! FTS maintenance is transactional with cache writes; old mail backfills in short batches.
use super::*;
pub(super) fn expression(query: &str) -> Result<String> {
    anyhow::ensure!(
        query.chars().count() <= 512,
        "Search may contain at most 512 characters"
    );
    let words = query
        .split_whitespace()
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .take(17)
        .collect::<Vec<_>>();
    anyhow::ensure!(words.len() <= 16, "Search may contain at most 16 words");
    Ok(words
        .into_iter()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND "))
}
fn index_sql(table: &str, staged: i32, account: &str, id: &str) -> String {
    format!("INSERT OR IGNORE INTO mail_search_keys(account,id,staged) VALUES({account},{id},{staged});
        DELETE FROM mail_search_fts WHERE rowid=(SELECT key_id FROM mail_search_keys WHERE account={account} AND id={id} AND staged={staged});
        INSERT INTO mail_search_fts(rowid,account,id,staged,sender,email,subject,body)
        SELECT k.key_id,m.account,m.id,{staged},coalesce(json_extract(m.data,'$.sender'),''),coalesce(json_extract(m.data,'$.email'),''),coalesce(json_extract(m.data,'$.subject'),''),coalesce(json_extract(m.data,'$.body'),'')
        FROM {table} m JOIN mail_search_keys k ON k.account=m.account AND k.id=m.id AND k.staged={staged} WHERE m.account={account} AND m.id={id};
        DELETE FROM mail_search_pending WHERE account={account} AND id={id} AND staged={staged};")
}
impl Store {
    pub(super) fn migrate_search(&mut self) -> Result<()> {
        if self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=5)",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(());
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_migrations WHERE version=5)",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            tx.commit()?;
            return Ok(());
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS mail_search_keys(key_id INTEGER PRIMARY KEY,account TEXT NOT NULL,id TEXT NOT NULL,staged INTEGER NOT NULL,UNIQUE(account,id,staged));
            CREATE TABLE IF NOT EXISTS mail_search_pending(account TEXT NOT NULL,id TEXT NOT NULL,staged INTEGER NOT NULL,PRIMARY KEY(account,id,staged));
            CREATE TABLE IF NOT EXISTS mail_pending_moves(account TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(account,id));
            CREATE VIRTUAL TABLE IF NOT EXISTS mail_search_fts USING fts5(account UNINDEXED,id UNINDEXED,staged UNINDEXED,sender,email,subject,body,tokenize='unicode61 remove_diacritics 2',prefix='2 3 4');")?;
        for (table, staged) in [("messages", 0), ("full_sync_messages", 1)] {
            for event in ["INSERT", "UPDATE"] {
                let when = if event == "UPDATE" {
                    "WHEN json_extract(old.data,'$.sender','$.email','$.subject','$.body') IS NOT json_extract(new.data,'$.sender','$.email','$.subject','$.body')"
                } else {
                    ""
                };
                tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS search_{table}_{event} AFTER {event} ON {table} {when} BEGIN {} END;",index_sql(table,staged,"new.account","new.id")))?;
            }
            tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS search_{table}_delete AFTER DELETE ON {table} BEGIN
                DELETE FROM mail_search_fts WHERE rowid=(SELECT key_id FROM mail_search_keys WHERE account=old.account AND id=old.id AND staged={staged});
                DELETE FROM mail_search_keys WHERE account=old.account AND id=old.id AND staged={staged};
                DELETE FROM mail_search_pending WHERE account=old.account AND id=old.id AND staged={staged}; END;
                INSERT OR IGNORE INTO mail_search_pending SELECT account,id,{staged} FROM {table};"))?;
        }
        tx.execute("INSERT INTO mail_migrations VALUES(5)", [])?;
        tx.commit()?;
        Ok(())
    }
    pub fn search_pending(&self) -> Result<u64> {
        Ok(self
            .0
            .query_row("SELECT count(*) FROM mail_search_pending", [], |r| r.get(0))?)
    }
    pub fn index_search_batch(&mut self) -> Result<usize> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = tx
            .prepare("SELECT account,id,staged FROM mail_search_pending LIMIT 32")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i32>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (account, id, staged) in &rows {
            // Bound identifiers come from the queue, never interpolated as SQL.
            tx.execute(
                "INSERT OR IGNORE INTO mail_search_keys(account,id,staged) VALUES(?1,?2,?3)",
                params![account, id, staged],
            )?;
            tx.execute("DELETE FROM mail_search_fts WHERE rowid=(SELECT key_id FROM mail_search_keys WHERE account=?1 AND id=?2 AND staged=?3)",params![account,id,staged])?;
            let table = if *staged == 1 {
                "full_sync_messages"
            } else {
                "messages"
            };
            tx.execute(&format!("INSERT INTO mail_search_fts(rowid,account,id,staged,sender,email,subject,body) SELECT k.key_id,m.account,m.id,?3,coalesce(json_extract(m.data,'$.sender'),''),coalesce(json_extract(m.data,'$.email'),''),coalesce(json_extract(m.data,'$.subject'),''),coalesce(json_extract(m.data,'$.body'),'') FROM {table} m JOIN mail_search_keys k ON k.account=m.account AND k.id=m.id AND k.staged=?3 WHERE m.account=?1 AND m.id=?2"),params![account,id,staged])?;
            tx.execute(
                "DELETE FROM mail_search_pending WHERE account=?1 AND id=?2 AND staged=?3",
                params![account, id, staged],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    }
    pub fn indexed_search(
        &self,
        query: &str,
        account: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let query = expression(query)?;
        if query.is_empty() {
            return Ok(vec![]);
        }
        let mut stmt=self.0.prepare("SELECT m.data FROM mailbox_summaries m WHERE (m.staged=1 OR NOT EXISTS(SELECT 1 FROM mailbox_summaries f WHERE f.account=m.account AND f.id=m.id AND f.staged=1)) AND (?2 IS NULL OR m.account=?2) AND (m.account,m.id,m.staged) IN (SELECT account,id,staged FROM mail_search_fts WHERE mail_search_fts MATCH ?1) ORDER BY m.timestamp DESC,m.account,m.id LIMIT ?3")?;
        let rows = stmt
            .query_map(params![query, account, limit.min(100) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fts_matches_full_bodies_accents_prefixes_and_not_query_operators() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut m = crate::fixtures::messages().remove(0);
        m.account = "me@example.com".into();
        m.body = format!(
            "{} A café meeting about the secretproject",
            "padding ".repeat(100)
        );
        s.snapshot(&m.account, &[m.clone()], "1").unwrap();
        assert_eq!(
            s.mailbox_page("All Inboxes", "cafe meet secretproj", 0)
                .unwrap()["total"],
            1
        );
        assert_eq!(
            s.indexed_search("secretproj", Some("other@example.com"), 100)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            s.mailbox_page("All Inboxes", "\" OR missingword", 0)
                .unwrap()["total"],
            0
        );
        assert_eq!(s.mailbox_page("All Inboxes", "***", 0).unwrap()["total"], 0);
        assert!(s.mailbox_page("All Inboxes", &"x".repeat(513), 0).is_err());
    }
    #[test]
    fn background_backfill_resumes_and_staged_overrides_never_return_stale_text() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let base = crate::fixtures::messages().remove(0);
        let messages = (0..65)
            .map(|i| {
                let mut m = base.clone();
                m.id = format!("m{i:03}");
                m.account = "me@example.com".into();
                m.body = format!("legacyword item{i}");
                m
            })
            .collect::<Vec<_>>();
        s.snapshot("me@example.com", &messages, "1").unwrap();
        s.0.execute_batch("DELETE FROM mail_search_fts; DELETE FROM mail_search_keys; INSERT INTO mail_search_pending SELECT account,id,0 FROM messages;").unwrap();
        assert_eq!(s.search_pending().unwrap(), 65);
        assert_eq!(s.index_search_batch().unwrap(), 32);
        assert_eq!(s.search_pending().unwrap(), 33);
        s.begin_full_sync("me@example.com", "2").unwrap();
        let mut changed = messages[0].clone();
        changed.body = "replacementword".into();
        s.stage_message("me@example.com", &changed).unwrap();
        while s.index_search_batch().unwrap() > 0 {}
        assert_eq!(
            s.mailbox_page("All Inboxes", "legacyword", 0).unwrap()["total"],
            64
        );
        assert_eq!(
            s.mailbox_page("All Inboxes", "replacementword", 0).unwrap()["total"],
            1
        );
        s.0.execute(
            "DELETE FROM full_sync_messages WHERE account=?1 AND id=?2",
            params![changed.account, changed.id],
        )
        .unwrap();
        assert_eq!(
            s.mailbox_page("All Inboxes", "replacementword", 0).unwrap()["total"],
            0
        );
        assert_eq!(
            s.mailbox_page("All Inboxes", "legacyword", 0).unwrap()["total"],
            65
        );
    }
}
