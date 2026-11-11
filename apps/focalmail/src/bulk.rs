//! Provider mutations are explicit and never retried. Cache updates follow confirmed responses.
use crate::{
    gmail_backend, imap_backend, outlook,
    providers::{self, Action},
    settings::Provider,
    store::Store,
    Message,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub account: String,
    pub id: String,
}
#[derive(Serialize, Deserialize)]
pub struct Failure {
    pub target: Target,
    pub detail: String,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Outcome {
    pub updated: Vec<Target>,
    pub failed: Vec<Failure>,
    pub not_attempted: Vec<Target>,
    pub warnings: Vec<String>,
    pub cancelled: bool,
}
enum Session {
    Gmail(gmail_backend::MutationSession),
    Outlook(outlook::MutationSession),
    Imap(imap_backend::MutationSession),
}
impl Session {
    fn open(account: &str) -> Result<Self> {
        match providers::profile(account)? {
            Some(p) if p.provider == Provider::Microsoft365 => {
                Ok(Self::Outlook(outlook::MutationSession::open(&p)?))
            }
            Some(p) if p.provider == Provider::Manual => {
                Ok(Self::Imap(imap_backend::MutationSession::open(&p)?))
            }
            _ => Ok(Self::Gmail(gmail_backend::MutationSession::open(account)?)),
        }
    }
    fn apply(&mut self, m: &Message, action: Action) -> Result<()> {
        match self {
            Self::Gmail(s) => s.apply(m, action),
            Self::Outlook(s) => s.apply(m, action),
            Self::Imap(s) => s.apply(m, action),
        }
    }
}
pub fn modify(
    targets: Vec<Target>,
    action: Action,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(&str),
) -> Result<Outcome> {
    let mut store = Store::open()?;
    let targets = validate(&store, targets)?;
    let mut sessions = BTreeMap::<String, Session>::new();
    let mut failed_sessions = BTreeMap::<String, String>::new();
    let mut outcome = perform(
        &mut store,
        targets,
        action,
        &cancel,
        |m, action| {
            if let Some(error) = failed_sessions.get(&m.account) {
                anyhow::bail!("{error}");
            }
            if !sessions.contains_key(&m.account) {
                match Session::open(&m.account) {
                    Ok(session) => {
                        sessions.insert(m.account.clone(), session);
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        failed_sessions.insert(m.account.clone(), detail.clone());
                        anyhow::bail!("{detail}");
                    }
                }
            }
            let session = sessions.get_mut(&m.account).unwrap();
            if let Err(error) = session.apply(m, action) {
                failed_sessions.insert(m.account.clone(), error.to_string());
                return Err(error);
            }
            Ok(matches!(session, Session::Imap(_))
                && matches!(action, Action::Archive | Action::Trash))
        },
        &progress,
    )?;
    let refresh = sessions
        .iter()
        .filter(|(account, session)| {
            matches!(session, Session::Imap(_))
                && outcome.updated.iter().any(|t| &t.account == *account)
                && matches!(action, Action::Archive | Action::Trash)
        })
        .map(|(a, _)| a.clone())
        .collect::<Vec<_>>();
    drop(sessions);
    for account in refresh {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress("Changes confirmed; refreshing IMAP folder identities…");
        let profile = match providers::profile(&account) {
            Ok(profile) => profile,
            Err(error) => {
                outcome.warnings.push(format!(
                    "Changes confirmed; IMAP folder refresh is pending: {error}"
                ));
                continue;
            }
        };
        if let Some(p) = profile {
            let _ = store.record_sync_started(&account);
            let result = imap_backend::sync(&p, &cancel);
            let _ = store.record_status(&account, &result);
            if let Err(error) = result {
                outcome.warnings.push(format!(
                    "Changes confirmed; IMAP folder refresh is pending: {error}"
                ));
            }
        }
    }
    Ok(outcome)
}
fn validate(store: &Store, targets: Vec<Target>) -> Result<Vec<Target>> {
    anyhow::ensure!(
        !targets.is_empty() && targets.len() <= 100,
        "Select between 1 and 100 messages"
    );
    let accounts = store.accounts()?;
    let unique = targets
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for t in &unique {
        anyhow::ensure!(
            accounts.contains(&t.account),
            "This account is no longer connected"
        );
        anyhow::ensure!(
            !store.move_pending(&t.account, &t.id)?,
            "An IMAP move is awaiting refresh; sync before changing that message again"
        );
        let message = store.cached_summary(&t.account, &t.id)?;
        anyhow::ensure!(
            message.account == t.account && message.id == t.id,
            "Cached message identity does not match its account"
        );
    }
    Ok(unique)
}
fn perform(
    store: &mut Store,
    targets: Vec<Target>,
    action: Action,
    cancel: &AtomicBool,
    mut remote: impl FnMut(&Message, Action) -> Result<bool>,
    progress: impl Fn(&str),
) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    let total = targets.len();
    for (index, target) in targets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            outcome.not_attempted.extend_from_slice(&targets[index..]);
            break;
        }
        let m = match store.cached_summary(&target.account, &target.id) {
            Ok(message) => message,
            Err(error) => {
                outcome.failed.push(Failure {
                    target: target.clone(),
                    detail: format!(
                        "No change attempted; cached message could not be read: {error}"
                    ),
                });
                continue;
            }
        };
        match remote(&m, action) {
            Ok(pending) => {
                outcome.updated.push(target.clone());
                if let Err(error) = store.apply_action(&target.account, &target.id, action, pending)
                {
                    outcome.warnings.push(format!(
                        "Change confirmed on server; cached view needs refresh: {error}"
                    ));
                }
                if pending
                    && !outcome
                        .warnings
                        .iter()
                        .any(|w| w.starts_with("IMAP folder"))
                {
                    outcome.warnings.push("IMAP folder identities will refresh during the next sync; moved mail remains readable from cache.".into());
                }
            }
            Err(error) => outcome.failed.push(Failure {
                target: target.clone(),
                detail: format!("Change not confirmed; refresh before retrying: {error}"),
            }),
        }
        progress(&format!(
            "Processed {} of {} selected messages",
            index + 1,
            total
        ));
    }
    Ok(outcome)
}
pub fn remove_account(account: &str) -> Result<String> {
    let mut store = Store::open()?;
    anyhow::ensure!(
        store.accounts()?.iter().any(|a| a == account),
        "Account is no longer connected"
    );
    let provider = store
        .connected_profile(account)?
        .map(|p| p.provider)
        .unwrap_or(Provider::Gmail);
    store.remove_account(account, || {
        match provider {
            Provider::Gmail => gmail_backend::forget_credentials(account),
            Provider::Microsoft365 => outlook::forget_credentials(account),
            Provider::Manual => imap_backend::forget_credentials(account),
        }
        .context("Could not clear sign-in from the keyring; unlock it and retry account removal")
    })?;
    Ok(
        "Account removed from this device. Server mail is unchanged; local drafts were retained."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Store, Vec<Target>) {
        let mut store = Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let mut messages = crate::fixtures::messages()
            .into_iter()
            .take(3)
            .collect::<Vec<_>>();
        for (i, m) in messages.iter_mut().enumerate() {
            m.account = "me@example.com".into();
            m.id = format!("m{i}");
            m.unread = true;
            m.labels = vec!["INBOX".into(), "UNREAD".into()];
        }
        store.snapshot("me@example.com", &messages, "1").unwrap();
        let targets = messages
            .into_iter()
            .map(|m| Target {
                account: m.account,
                id: m.id,
            })
            .collect();
        (store, targets)
    }
    #[test]
    fn cancellation_and_partial_failures_preserve_per_message_outcomes() {
        let (mut store, targets) = setup();
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let result = perform(
            &mut store,
            targets,
            Action::Read(true),
            &cancel,
            |_, _| {
                calls += 1;
                if calls == 2 {
                    cancel.store(true, Ordering::Relaxed);
                    anyhow::bail!("simulated provider failure");
                }
                Ok(false)
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result.updated.len(), 1);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.not_attempted.len(), 1);
        assert!(result.cancelled);
        assert!(!store.cached_message("me@example.com", "m0").unwrap().unread);
        assert!(store.cached_message("me@example.com", "m1").unwrap().unread);
        assert!(store.cached_message("me@example.com", "m2").unwrap().unread);
    }
    #[test]
    fn preflight_rejects_unknown_messages_limits_and_pending_moves() {
        let (mut store, targets) = setup();
        assert_eq!(
            validate(&store, vec![targets[0].clone(), targets[0].clone()])
                .unwrap()
                .len(),
            1
        );
        assert!(validate(&store, vec![]).is_err());
        assert!(validate(&store, vec![targets[0].clone(); 101]).is_err());
        assert!(validate(
            &store,
            vec![Target {
                account: "other@example.com".into(),
                id: "m0".into()
            }]
        )
        .is_err());
        let original = store.cached_message("me@example.com", "m0").unwrap().body;
        store
            .apply_action("me@example.com", "m0", Action::Archive, true)
            .unwrap();
        assert_eq!(
            store.cached_message("me@example.com", "m0").unwrap().body,
            original
        );
        assert_eq!(
            store.mailbox_page("Archive", "", 0).unwrap()["messages"][0]["pending_move"],
            true
        );
        assert!(validate(&store, vec![targets[0].clone()]).is_err());
        let mut refreshed = store.cached_message("me@example.com", "m0").unwrap();
        refreshed.id = "new-folder-uid".into();
        store.snapshot("me@example.com", &[refreshed], "2").unwrap();
        assert!(!store.move_pending("me@example.com", "m0").unwrap());
    }
}
