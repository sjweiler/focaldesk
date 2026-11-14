//! Narrow diagnostics operations; no log contents enter the webview.
use crate::preferences::{read_document, reload_message, write_document};
use anyhow::{bail, Context, Result};
use focaldesk_settings_core::DebugSettings;
use gio::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct DebugSnapshot {
    pub preferences: DebugSettings,
    pub defaults: DebugSettings,
    pub log_path: Option<String>,
    pub diagnostics: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugChange {
    pub expected: DebugSettings,
    pub requested: DebugSettings,
}
#[derive(Serialize)]
pub struct DebugResult {
    pub snapshot: DebugSnapshot,
    pub message: String,
}
fn read(document: &Value) -> Result<DebugSettings> {
    document
        .get("debug")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("Debug preferences are malformed; nothing was changed.")
        .map(Option::unwrap_or_default)
}
fn save_at(path: &Path, change: DebugChange) -> Result<DebugSettings> {
    let mut document = read_document(path)?;
    if read(&document)? != change.expected {
        bail!("Debug preferences changed in another client. Reload before applying.");
    }
    let target = document
        .as_object_mut()
        .unwrap()
        .entry("debug")
        .or_insert_with(|| serde_json::json!({}));
    target
        .as_object_mut()
        .context("Debug preferences are malformed")?
        .extend(
            serde_json::to_value(&change.requested)?
                .as_object()
                .unwrap()
                .clone(),
        );
    write_document(path, &document)?;
    Ok(change.requested)
}
pub fn log_path() -> Option<PathBuf> {
    let mut candidates = focaldesk_logging::log_file_path_candidates();
    if let Some(state) = dirs::state_dir() {
        candidates.insert(
            1.min(candidates.len()),
            state.join("focaldesk/focaldesk.log"),
        );
    }
    candidates
        .into_iter()
        .find(|path| path.is_absolute() && path.is_file())
}
fn env_label(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| "unset".into())
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect()
}
pub fn diagnostics() -> String {
    let session = std::env::var("XDG_SESSION_TYPE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                "wayland".into()
            } else if std::env::var_os("DISPLAY").is_some() {
                "x11".into()
            } else {
                "unknown".into()
            }
        });
    format!("FocalDesk diagnostics\nVersion: {}\nBuild hash: {}\nBuild profile: {}\nOS/arch: {}/{}\nSession type: {}\nCurrent executable: {}\nSettings path: {}\nLog path: {}\nWAYLAND_DISPLAY: {}\nDISPLAY: {}\nXDG_CURRENT_DESKTOP: {}",
        env!("CARGO_PKG_VERSION"),option_env!("VERGEN_GIT_SHA").unwrap_or("development"),
        if cfg!(debug_assertions){"debug"}else{"release"},std::env::consts::OS,std::env::consts::ARCH,session,
        std::env::current_exe().map(|path|path.display().to_string()).unwrap_or_else(|_|"unknown".into()),
        focaldesk_settings_core::settings_path().display(),log_path().map(|path|path.display().to_string()).unwrap_or_else(||"not found".into()),
        env_label("WAYLAND_DISPLAY"),env_label("DISPLAY"),env_label("XDG_CURRENT_DESKTOP"))
}
fn with_diagnostics(preferences: DebugSettings) -> DebugSnapshot {
    DebugSnapshot {
        preferences,
        defaults: DebugSettings::default(),
        log_path: log_path().map(|path| path.display().to_string()),
        diagnostics: diagnostics(),
    }
}
pub fn snapshot() -> Result<DebugSnapshot> {
    Ok(with_diagnostics(read(&read_document(
        &focaldesk_settings_core::settings_path(),
    )?)?))
}
pub fn apply(change: DebugChange) -> Result<DebugResult> {
    let preferences = save_at(&focaldesk_settings_core::settings_path(), change)?;
    Ok(DebugResult {
        snapshot: with_diagnostics(preferences),
        message: reload_message(),
    })
}
pub fn open_log() -> Result<String> {
    let path = log_path().context("No FocalDesk log file was found.")?;
    let uri = gio::File::for_path(&path).uri();
    gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
        .context("Could not request opening the FocalDesk log file")?;
    Ok("Requested opening the log file in its default application.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_transaction_preserves_future_fields_and_rejects_stale_changes() {
        let path =
            std::env::temp_dir().join(format!("debug-preferences-{}.json", std::process::id()));
        let initial = serde_json::json!({"debug":{"future":42},"other":{"keep":true}});
        std::fs::write(&path, initial.to_string()).unwrap();
        let expected = read(&initial).unwrap();
        let requested = DebugSettings {
            show_fps: true,
            ..Default::default()
        };
        save_at(
            &path,
            DebugChange {
                expected: expected.clone(),
                requested: requested.clone(),
            },
        )
        .unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(saved["debug"]["future"], 42);
        assert_eq!(saved["other"], initial["other"]);
        let bytes = std::fs::read(&path).unwrap();
        assert!(save_at(
            &path,
            DebugChange {
                expected,
                requested
            }
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, "{broken").unwrap();
        assert!(save_at(
            &path,
            DebugChange {
                expected: DebugSettings::default(),
                requested: DebugSettings::default()
            }
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
        std::fs::remove_file(path).unwrap();
    }
}
