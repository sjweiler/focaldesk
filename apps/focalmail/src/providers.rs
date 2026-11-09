//! Provider dispatch. Legacy accounts without a provider marker remain Gmail.
pub use crate::gmail_backend::Action;
use crate::{
    attachments::{Attachment, DraftAttachment},
    drafts::{Draft, ForwardSource},
    settings::{Profile, Provider},
    store::Store,
    Message,
};
use anyhow::{Context, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
pub fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::service::OperationCancelled.into());
    }
    Ok(())
}
pub fn profile(email: &str) -> Result<Option<Profile>> {
    Store::open()?.connected_profile(email)
}
pub fn ensure_provider(p: &Profile) -> Result<()> {
    let store = Store::open()?;
    if store.accounts()?.contains(&p.email) {
        let existing = store
            .connected_profile(&p.email)?
            .map(|p| p.provider)
            .unwrap_or(Provider::Gmail);
        anyhow::ensure!(
            existing == p.provider,
            "This address is already connected using another provider"
        );
    }
    Ok(())
}
pub fn connect_profile(
    profile: &Profile,
    password: &str,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(&str),
) -> Result<String> {
    profile.validate()?;
    ensure_provider(profile)?;
    match profile.provider {
        Provider::Microsoft365 => crate::outlook::connect(profile, cancel, progress),
        Provider::Manual => crate::imap_backend::connect(profile, password, cancel),
        Provider::Gmail => anyhow::bail!("Use Google sign-in"),
    }
}
pub use crate::gmail_backend::connect;
pub fn reconnect(email: &str, cancel: Arc<AtomicBool>, progress: impl Fn(&str)) -> Result<String> {
    match profile(email)? {
        Some(p) if p.provider == Provider::Microsoft365 => {
            crate::outlook::connect(&p, cancel, progress)
        }
        Some(p) if p.provider == Provider::Manual => {
            let result = crate::imap_backend::sync(&p, &cancel);
            Store::open()?.record_status(email, &result)?;
            result?;
            Ok("Account connected".into())
        }
        _ => crate::gmail_backend::reconnect(email, cancel, progress),
    }
}
pub fn sync_all(cancel: Arc<AtomicBool>) -> Result<String> {
    sync_accounts(cancel, false)
}
pub fn sync_background(cancel: Arc<AtomicBool>) -> Result<String> {
    sync_accounts(cancel, true)
}
fn sync_accounts(cancel: Arc<AtomicBool>, background: bool) -> Result<String> {
    let store = Store::open()?;
    let accounts = if background {
        store.background_accounts()?
    } else {
        store.accounts()?
    };
    anyhow::ensure!(
        background || !accounts.is_empty(),
        "Connect a mail account first"
    );
    // Separate connections and workers keep a long Gmail import from blocking
    // another account. The service retains its operation lock until all workers
    // exit, so interactive mutations and cancellation remain serialized safely.
    let results = sync_independently(&accounts, &cancel, sync_one_account);
    let mut failures = Vec::new();
    for (email, result) in accounts.iter().zip(results) {
        if let Err(error) = result {
            if error.is::<crate::service::OperationCancelled>() {
                return Err(error);
            }
            failures.push(format!("{email}: {error}"));
        }
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(format!("Synced {} account(s)", accounts.len()))
}
fn sync_independently(
    accounts: &[String],
    cancel: &AtomicBool,
    sync: impl Fn(&str, &AtomicBool) -> Result<()> + Sync,
) -> Vec<Result<()>> {
    std::thread::scope(|scope| {
        let workers = accounts
            .iter()
            .map(|email| {
                let sync = &sync;
                scope.spawn(move || sync(email, cancel))
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Account sync worker failed")))
            })
            .collect::<Vec<_>>()
    })
}
fn sync_one_account(email: &str, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    let store = Store::open()?;
    let initialized = !store.history(email)?.is_empty();
    store.record_sync_started(email)?;
    let before = if initialized {
        store.cached_ids(email)?
    } else {
        Default::default()
    };
    let result = match profile(email)? {
        Some(p) if p.provider == Provider::Microsoft365 => crate::outlook::sync(&p, cancel),
        Some(p) if p.provider == Provider::Manual => crate::imap_backend::sync(&p, cancel),
        _ => crate::gmail_backend::sync_account(email, cancel),
    };
    store.record_status(email, &result)?;
    result?;
    if initialized {
        crate::notifications::new_mail(email, &before);
    }
    Ok(())
}

pub fn modify(message: &Message, action: Action, cancel: Arc<AtomicBool>) -> Result<String> {
    cancelled(&cancel)?;
    match profile(&message.account)? {
        Some(p) if p.provider == Provider::Microsoft365 => {
            crate::outlook::modify(&p, message, action, &cancel)
        }
        Some(p) if p.provider == Provider::Manual => {
            crate::imap_backend::modify(&p, message, action, &cancel)
        }
        _ => crate::gmail_backend::modify(message, action, cancel),
    }
}
pub fn send(draft: &Draft, files: &[DraftAttachment], cancel: Arc<AtomicBool>) -> Result<String> {
    anyhow::ensure!(
        Store::open()?.accounts()?.contains(&draft.from),
        "Choose a connected sender account before sending this draft"
    );
    cancelled(&cancel)?;
    match profile(&draft.from)? {
        Some(p) if p.provider != Provider::Gmail => {
            // Shared MIME validation covers header injection, attachments and reply context.
            let raw = mime(draft, files)?;
            let outcome = if p.provider == Provider::Microsoft365 {
                crate::outlook::send(&p, &raw, &cancel)?;
                "Message sent · background sync will update Sent".into()
            } else {
                crate::imap_backend::send(&p, draft, &raw, &cancel)?
            };
            if Store::open()
                .and_then(|s| s.delete_local_draft(&draft.id))
                .is_err()
            {
                return Ok("Message sent; saved draft could not be cleared. Do not resend.".into());
            }
            Ok(outcome)
        }
        _ => crate::gmail_backend::send(draft, files, cancel),
    }
}
pub fn mime(draft: &Draft, files: &[DraftAttachment]) -> Result<Vec<u8>> {
    use base64::Engine;
    let payload = crate::gmail_backend::draft_message(draft, files)?;
    let mut bytes = format!(
        "Date: {}\r\nMessage-ID: <{:032x}@focalmail.local>\r\n",
        chrono::Utc::now().to_rfc2822(),
        rand::random::<u128>()
    )
    .into_bytes();
    bytes.extend(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload["raw"].as_str().context("Missing MIME")?)?,
    );
    Ok(bytes)
}
pub fn load_content(message: &Message) -> Result<Message> {
    if message.attachments_loaded
        && message.html_loaded
        && message.html.as_ref().is_none_or(|h| !h.needs_reload())
    {
        return Ok(message.clone());
    }
    let loaded = match profile(&message.account)? {
        Some(p) if p.provider == Provider::Microsoft365 => {
            crate::outlook::load_content(&p, message)
        }
        Some(p) if p.provider == Provider::Manual => crate::imap_backend::load_content(&p, message),
        _ => crate::gmail_backend::load_content(message),
    }?;
    Store::open()?.cache_content(&loaded)?;
    Ok(loaded)
}
fn attachment_bytes(message: &Message, attachment: &Attachment) -> Result<Vec<u8>> {
    let loaded = load_content(message)?;
    let a = loaded
        .attachments
        .iter()
        .find(|a| a.id == attachment.id)
        .context("Attachment unavailable")?;
    use base64::Engine;
    anyhow::ensure!(
        a.size <= crate::attachments::MAX_BYTES as u64,
        "Attachment exceeds 20 MiB"
    );
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&a.data)?;
    anyhow::ensure!(
        bytes.len() <= crate::attachments::MAX_BYTES,
        "Attachment exceeds 20 MiB"
    );
    Ok(bytes)
}
pub fn download(message: &Message, attachment: &Attachment) -> Result<String> {
    if profile(&message.account)?.is_none_or(|p| p.provider == Provider::Gmail) {
        return crate::gmail_backend::download(message, attachment);
    }
    let bytes = attachment_bytes(message, attachment)?;
    let directory = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|p| p.join("Downloads")))
        .context("Cannot locate Downloads")?;
    Ok(format!(
        "Saved attachment to {}",
        crate::attachments::save_download(&directory, &attachment.name, &bytes)?.display()
    ))
}
pub fn forward_files(
    source: &ForwardSource,
    selected: &[usize],
    remaining: usize,
) -> Result<Vec<DraftAttachment>> {
    if profile(&source.account)?.is_none_or(|p| p.provider == Provider::Gmail) {
        return crate::gmail_backend::forward_files(source, selected, remaining);
    }
    let message = Store::open()?
        .load()?
        .into_iter()
        .find(|m| m.account == source.account && m.id == source.message_id)
        .context("Message unavailable")?;
    let mut files = Vec::new();
    let mut used = 0;
    for index in selected {
        let a = source
            .attachments
            .get(*index)
            .context("Attachment unavailable")?;
        let bytes = attachment_bytes(&message, a)?;
        used += bytes.len();
        anyhow::ensure!(
            used <= remaining.min(crate::attachments::MAX_BYTES),
            "Attachments exceed remaining budget"
        );
        files.push(DraftAttachment {
            name: a.name.clone(),
            data: bytes,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    #[test]
    fn a_slow_account_does_not_block_another_and_both_observe_cancellation() {
        let accounts = vec!["slow-gmail".to_string(), "microsoft".to_string()];
        let cancel = AtomicBool::new(false);
        let (started, observed) = std::sync::mpsc::channel();
        let observed = std::sync::Mutex::new(observed);
        let results = sync_independently(&accounts, &cancel, |account, cancel| {
            if account == "slow-gmail" {
                observed
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                assert!(cancel.load(Ordering::Relaxed));
            } else {
                cancel.store(true, Ordering::Relaxed);
                started.send(()).unwrap();
            }
            cancelled(cancel)
        });
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r
            .as_ref()
            .unwrap_err()
            .is::<crate::service::OperationCancelled>()));
    }
}
