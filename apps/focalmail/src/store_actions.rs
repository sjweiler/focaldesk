use super::*;
use crate::providers::Action;
impl Store {
    pub fn cached_summary(&self, account: &str, id: &str) -> Result<Message> {
        let data:String=self.0.query_row("SELECT data FROM mailbox_summaries WHERE account=?1 AND id=?2 ORDER BY staged DESC LIMIT 1",params![account,id],|r|r.get(0)).context("Message is no longer cached")?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn move_pending(&self, account: &str, id: &str) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_pending_moves WHERE account=?1 AND id=?2)",
            params![account, id],
            |r| r.get(0),
        )?)
    }
    pub fn apply_action(
        &mut self,
        account: &str,
        id: &str,
        action: Action,
        pending_move: bool,
    ) -> Result<()> {
        let mut m = self.cached_summary(account, id)?;
        let has_labels = !m.labels.is_empty();
        match action {
            Action::Read(read) => {
                m.unread = !read;
                m.labels.retain(|l| l != "UNREAD");
                if !read && has_labels {
                    m.labels.push("UNREAD".into());
                }
            }
            Action::Star(star) => {
                m.flagged = star;
                m.labels.retain(|l| l != "STARRED");
                if star && has_labels {
                    m.labels.push("STARRED".into());
                }
            }
            Action::Archive => {
                m.labels.retain(|l| l != "INBOX");
                m.folder = if m.labels.iter().any(|l| l == "SENT") {
                    "Sent"
                } else {
                    "Archive"
                }
                .into();
            }
            Action::Trash => {
                m.labels.retain(|l| l != "INBOX");
                if has_labels && !m.labels.iter().any(|l| l == "TRASH") {
                    m.labels.push("TRASH".into());
                }
                m.folder = "Trash".into();
            }
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in ["messages", "full_sync_messages"] {
            tx.execute(&format!("UPDATE {table} SET data=json_set(data,'$.unread',json(?3),'$.flagged',json(?4),'$.folder',?5,'$.labels',json(?6)) WHERE account=?1 AND id=?2"),params![account,id,serde_json::to_string(&m.unread)?,serde_json::to_string(&m.flagged)?,m.folder,serde_json::to_string(&m.labels)?])?;
        }
        if pending_move {
            tx.execute(
                "INSERT OR IGNORE INTO mail_pending_moves VALUES(?1,?2)",
                params![account, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn remove_account(
        &mut self,
        account: &str,
        forget: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in [
            "messages",
            "full_sync_messages",
            "sync_state",
            "full_sync_state",
            "mail_sync_progress",
            "mail_pending_moves",
            "mail_signatures",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE account=?1"), [account])?;
        }
        tx.execute("DELETE FROM connected_providers WHERE email=?1", [account])?;
        for table in ["account_status", "account_profiles", "accounts"] {
            tx.execute(&format!("DELETE FROM {table} WHERE email=?1"), [account])?;
        }
        tx.execute("DELETE FROM mailbox_summaries WHERE account=?1", [account])?;
        tx.execute("DELETE FROM mail_search_fts WHERE account=?1", [account])?;
        tx.execute("DELETE FROM mail_search_keys WHERE account=?1", [account])?;
        tx.execute(
            "DELETE FROM mail_search_pending WHERE account=?1",
            [account],
        )?;
        tx.execute("DELETE FROM draft_sender WHERE account=?1", [account])?;
        forget()?;
        tx.commit().context(
            "Sign-in cleared, but cache cleanup could not commit; retry account removal",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removal_rolls_back_on_keyring_failure_then_clears_only_one_accounts_cache() {
        let mut s = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut a = crate::fixtures::messages().remove(0);
        a.account = "first@example.com".into();
        a.body = "firstprivateword".into();
        let mut b = a.clone();
        b.account = "second@example.com".into();
        b.body = "secondprivateword".into();
        s.snapshot(&a.account, &[a.clone()], "1").unwrap();
        s.snapshot(&b.account, &[b.clone()], "1").unwrap();
        s.begin_full_sync(&a.account, "2").unwrap();
        s.stage_message(&a.account, &a).unwrap();
        let draft = crate::drafts::Draft::new(a.account.clone());
        let files = [crate::attachments::DraftAttachment {
            name: "keep.txt".into(),
            data: vec![1, 2, 3],
        }];
        s.save_local_draft(&draft, Some(&files)).unwrap();
        assert!(s
            .remove_account(&a.account, || anyhow::bail!("keyring locked"))
            .is_err());
        assert_eq!(s.accounts().unwrap().len(), 2);
        assert_eq!(
            s.mailbox_page("All Inboxes", "firstprivateword", 0)
                .unwrap()["total"],
            1
        );
        s.remove_account(&a.account, || Ok(())).unwrap();
        assert_eq!(s.accounts().unwrap(), vec![b.account.clone()]);
        assert!(s.cached_message(&a.account, &a.id).is_err());
        assert_eq!(
            s.mailbox_page("All Inboxes", "firstprivateword", 0)
                .unwrap()["total"],
            0
        );
        assert_eq!(
            s.mailbox_page("All Inboxes", "secondprivateword", 0)
                .unwrap()["total"],
            1
        );
        let (saved, files) = s.load_local_draft(&draft.id).unwrap();
        assert_eq!(saved.from, a.account);
        assert_eq!(files[0].data, vec![1, 2, 3]);
    }
}
