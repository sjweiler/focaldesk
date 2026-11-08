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
            crate::imap_backend::sync(&p, &cancel)?;
            Ok("Account connected".into())
        }
        _ => crate::gmail_backend::reconnect(email, cancel, progress),
    }
}
pub fn sync_all(cancel: Arc<AtomicBool>) -> Result<String> {
    let store = Store::open()?;
    let accounts = store.accounts()?;
    anyhow::ensure!(!accounts.is_empty(), "Connect a mail account first");
    let mut failures = Vec::new();
    for email in &accounts {
        cancelled(&cancel)?;
        let initialized = !store.history(email)?.is_empty();
        let before = store
            .load()?
            .into_iter()
            .filter(|m| m.account == *email)
            .map(|m| m.id)
            .collect();
        let result = match profile(email)? {
            Some(p) if p.provider == Provider::Microsoft365 => crate::outlook::sync(&p, &cancel),
            Some(p) if p.provider == Provider::Manual => crate::imap_backend::sync(&p, &cancel),
            _ => crate::gmail_backend::sync_account(email, &cancel),
        };
        store.record_status(email, &result)?;
        if let Err(e) = result {
            failures.push(format!("{email}: {e}"));
        } else if initialized {
            crate::notifications::new_mail(email, &before);
        }
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(format!("Synced {} account(s)", accounts.len()))
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
    if message.attachments_loaded && message.html_loaded {
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
