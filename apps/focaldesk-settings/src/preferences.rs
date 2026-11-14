//! Narrow, typed Privacy and Power operations. Preserve the rest of settings.json.
use anyhow::{bail, Context, Result};
use focaldesk_ipc::{
    send_desktop_request, send_power_request, send_settings_request, IpcRequest, IpcResponse,
    PowerIpcRequest, PowerIpcResponse,
};
use focaldesk_settings_core::{PerformanceMode, PowerSettings, PrivacySettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "section", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreferenceChange {
    Privacy {
        expected: PrivacySettings,
        requested: PrivacySettings,
    },
    Power {
        expected: PowerSettings,
        requested: PowerSettings,
    },
}

#[derive(Serialize)]
pub struct Preferences {
    privacy: PrivacySettings,
    power: PowerSettings,
}

#[derive(Serialize)]
pub struct ApplyResult {
    preferences: Preferences,
    message: String,
}

pub(crate) fn read_document(path: &Path) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)
                .context("Settings contains invalid JSON; no changes were saved.")?;
            if !value.is_object() {
                bail!("Settings must contain a JSON object.");
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(error.into()),
    }
}

fn preferences(document: &Value) -> Result<Preferences> {
    Ok(Preferences {
        privacy: document
            .get("privacy")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default(),
        power: document
            .get("power")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default(),
    })
}

pub fn snapshot() -> Result<Preferences> {
    preferences(&read_document(&focaldesk_settings_core::settings_path())?)
}

fn save_at(path: &Path, change: &PreferenceChange) -> Result<Preferences> {
    let mut document = read_document(path)?;
    let current = preferences(&document)?;
    let (section, expected, requested, actual) = match change {
        PreferenceChange::Privacy {
            expected,
            requested,
        } => {
            if requested.notification_history_limit != expected.notification_history_limit
                && !matches!(requested.notification_history_limit, 25 | 50 | 100)
            {
                bail!("Choose a notification history limit of 25, 50, or 100 entries.");
            }
            (
                "privacy",
                serde_json::to_value(expected)?,
                serde_json::to_value(requested)?,
                serde_json::to_value(current.privacy)?,
            )
        }
        PreferenceChange::Power {
            expected,
            requested,
        } => {
            if (requested.blank_screen_minutes != expected.blank_screen_minutes
                && !matches!(requested.blank_screen_minutes, None | Some(1 | 5 | 10 | 30)))
                || (requested.suspend_minutes != expected.suspend_minutes
                    && !matches!(requested.suspend_minutes, None | Some(15 | 30 | 60)))
            {
                bail!("Choose one of the supported idle timeouts.");
            }
            (
                "power",
                serde_json::to_value(expected)?,
                serde_json::to_value(requested)?,
                serde_json::to_value(current.power)?,
            )
        }
    };
    if actual != expected {
        bail!("These preferences changed in another client. Reload before applying.");
    }
    let target = document
        .as_object_mut()
        .unwrap()
        .entry(section)
        .or_insert_with(|| serde_json::json!({}));
    if !target.is_object() {
        bail!("The settings section is malformed.");
    }
    // Only known fields are replaced. Future keys and all other sections survive.
    target
        .as_object_mut()
        .unwrap()
        .extend(requested.as_object().unwrap().clone());
    write_document(path, &document)?;
    preferences(&document)
}

pub(crate) fn write_document(path: &Path, document: &Value) -> Result<()> {
    let parent = path.parent().context("Missing settings directory")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".preferences-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&document)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn apply(change: PreferenceChange) -> Result<ApplyResult> {
    let preferences = save_at(&focaldesk_settings_core::settings_path(), &change)?;
    let mut message = reload_message();
    if let PreferenceChange::Privacy { requested, .. } = &change {
        match focaldesk_ipc::send_notification_request(&focaldesk_ipc::NotificationIpcRequest::SetHistoryLimit { limit: requested.notification_history_limit }) {
            Ok(focaldesk_ipc::NotificationIpcResponse::Ok) => {},
            Ok(focaldesk_ipc::NotificationIpcResponse::Error { message: error }) => message.push_str(&format!(" Preferences saved, but live notification retention failed: {error}")),
            _ => message.push_str(" Preferences saved, but the notification service did not acknowledge the live retention update. Apply again to retry."),
        }
    }
    if let PreferenceChange::Power {
        expected,
        requested,
    } = change
    {
        if expected.performance_mode != requested.performance_mode {
            let profile = match requested.performance_mode {
                PerformanceMode::Balanced => "balanced",
                PerformanceMode::Performance => "performance",
                PerformanceMode::PowerSaver => "power-saver",
            };
            if !matches!(
                send_power_request(&PowerIpcRequest::SetPerformanceProfile {
                    profile: profile.into()
                }),
                Ok(PowerIpcResponse::Ok)
            ) {
                message.push_str(" The requested performance profile was saved but could not be activated by the power service.");
            }
        }
    }
    Ok(ApplyResult {
        preferences,
        message,
    })
}

pub(crate) fn reload_message() -> String {
    let _ = send_settings_request(&IpcRequest::Reload);
    if matches!(
        send_desktop_request(&IpcRequest::Reload),
        Ok(IpcResponse::Ok)
    ) {
        "Preferences saved and the desktop reloaded.".into()
    } else {
        "Preferences saved. The desktop could not reload them; they will apply next session.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_other_sections_and_future_fields_rejects_stale_invalid_and_malformed() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-preferences-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        fs::write(&path, r#"{"privacy":{"future":42},"input":{"keyboard_layout":"de"},"displays":{"future":true}}"#).unwrap();
        let expected = PrivacySettings::default();
        let mut requested = expected.clone();
        requested.recent_files = !expected.recent_files;
        let change = PreferenceChange::Privacy {
            expected: expected.clone(),
            requested,
        };
        save_at(&path, &change).unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(saved["privacy"]["future"], 42);
        assert_eq!(saved["input"]["keyboard_layout"], "de");
        assert_eq!(saved["displays"]["future"], true);
        let bytes = fs::read(&path).unwrap();
        assert!(save_at(&path, &change).is_err());
        let invalid = PowerSettings {
            suspend_minutes: Some(0),
            ..PowerSettings::default()
        };
        assert!(save_at(
            &path,
            &PreferenceChange::Power {
                expected: PowerSettings::default(),
                requested: invalid
            }
        )
        .is_err());
        assert_eq!(bytes, fs::read(&path).unwrap());
        fs::write(&path, "bad json").unwrap();
        assert!(save_at(&path, &change).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "bad json");
        fs::remove_dir_all(root).unwrap();
    }
}
