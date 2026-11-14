//! Workspace controls, excluding compositor-owned saved split ratios.
use crate::preferences::{read_document, reload_message, write_document};
use anyhow::{bail, Context, Result};
use focaldesk_settings_core::{WorkspaceSettings, WorkspaceTransitionStyle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePreferences {
    pub restore_session: bool,
    pub restore_split_layouts: bool,
    pub maximize_on_launch: bool,
    pub split_screen_enabled: bool,
    pub max_workspace_slots: u32,
    pub workspace_names: BTreeMap<u32, String>,
    pub workspace_wallpapers: BTreeMap<u32, String>,
    pub transition_style: WorkspaceTransitionStyle,
}
impl From<WorkspaceSettings> for WorkspacePreferences {
    fn from(value: WorkspaceSettings) -> Self {
        Self {
            restore_session: value.restore_session,
            restore_split_layouts: value.restore_split_layouts,
            maximize_on_launch: value.maximize_on_launch,
            split_screen_enabled: value.split_screen_enabled,
            max_workspace_slots: value.max_workspace_slots,
            workspace_names: value.workspace_names,
            workspace_wallpapers: value.workspace_wallpapers,
            transition_style: value.transition_style,
        }
    }
}
#[derive(Serialize)]
pub struct WorkspaceSnapshot {
    pub preferences: WorkspacePreferences,
    pub defaults: WorkspacePreferences,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceChange {
    pub expected: WorkspacePreferences,
    pub requested: WorkspacePreferences,
}
#[derive(Serialize)]
pub struct WorkspaceResult {
    pub snapshot: WorkspaceSnapshot,
    pub message: String,
}
fn read(document: &Value) -> Result<WorkspacePreferences> {
    document
        .get("workspaces")
        .cloned()
        .map(serde_json::from_value::<WorkspaceSettings>)
        .transpose()
        .context("Workspace settings are malformed; nothing was changed.")
        .map(|value| value.unwrap_or_default().into())
}
fn validate(change: &WorkspaceChange) -> Result<()> {
    let old = &change.expected;
    let new = &change.requested;
    if new.max_workspace_slots != old.max_workspace_slots
        && !(1..=9).contains(&new.max_workspace_slots)
    {
        bail!("Choose from 1 to 9 visible workspace slots.");
    }
    for (before, after, wallpaper) in [
        (&old.workspace_names, &new.workspace_names, false),
        (&old.workspace_wallpapers, &new.workspace_wallpapers, true),
    ] {
        for id in before.keys().chain(after.keys()) {
            if before.get(id) == after.get(id) {
                continue;
            }
            if !(1..=9).contains(id) {
                bail!("Only workspace entries 1 through 9 can be edited here.");
            }
            if let Some(value) = after.get(id) {
                if value.is_empty()
                    || value.trim() != value
                    || value.chars().any(char::is_control)
                    || value.chars().count() > if wallpaper { 4096 } else { 128 }
                {
                    bail!("Use nonempty text without surrounding whitespace or control characters (names: 128 characters; paths: 4096).");
                }
                if wallpaper {
                    let path = Path::new(value);
                    let extension = path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if !path.is_absolute()
                        || !path.is_file()
                        || !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "exr")
                    {
                        bail!("Choose an existing local PNG, JPEG, WebP, or EXR wallpaper using its absolute path.");
                    }
                }
            }
        }
    }
    Ok(())
}
fn save_at(path: &Path, change: WorkspaceChange) -> Result<WorkspacePreferences> {
    validate(&change)?;
    let mut document = read_document(path)?;
    if read(&document)? != change.expected {
        bail!("Workspace preferences changed in another client. Reload before applying.");
    }
    let requested = serde_json::to_value(&change.requested)?;
    let target = document
        .as_object_mut()
        .unwrap()
        .entry("workspaces")
        .or_insert_with(|| serde_json::json!({}));
    let target = target
        .as_object_mut()
        .context("Workspace settings are malformed")?;
    // split_ratios and future fields remain untouched; maps include unedited IDs.
    target.extend(requested.as_object().unwrap().clone());
    write_document(path, &document)?;
    Ok(change.requested)
}
fn with_defaults(preferences: WorkspacePreferences) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        preferences,
        defaults: WorkspaceSettings::default().into(),
    }
}
pub fn snapshot() -> Result<WorkspaceSnapshot> {
    Ok(with_defaults(read(&read_document(
        &focaldesk_settings_core::settings_path(),
    )?)?))
}
pub fn apply(change: WorkspaceChange) -> Result<WorkspaceResult> {
    let preferences = save_at(&focaldesk_settings_core::settings_path(), change)?;
    Ok(WorkspaceResult {
        snapshot: with_defaults(preferences),
        message: format!(
            "{} Session and split-layout restoration take effect at the next sign-in; maximize-on-launch affects new windows.",
            reload_message()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_split_state_future_fields_and_unlisted_workspaces() {
        let path =
            std::env::temp_dir().join(format!("workspace-preferences-{}.json", std::process::id()));
        let initial = serde_json::json!({"apps":{"keep":true},"workspaces":{"future":42,"split_ratios":{"DP-1":{"side_by_side":0.4,"stacked":0.6,"future":true}},"workspace_names":{"12":"Keep"},"workspace_wallpapers":{"12":"missing-legacy.exr"}}});
        std::fs::write(&path, initial.to_string()).unwrap();
        let expected = read(&initial).unwrap();
        let mut requested = expected.clone();
        requested.workspace_names.insert(1, "Work".into());
        requested.max_workspace_slots = 5;
        save_at(
            &path,
            WorkspaceChange {
                expected: expected.clone(),
                requested: requested.clone(),
            },
        )
        .unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(
            saved["workspaces"]["split_ratios"],
            initial["workspaces"]["split_ratios"]
        );
        assert_eq!(saved["workspaces"]["future"], 42);
        assert_eq!(saved["apps"], initial["apps"]);
        assert_eq!(saved["workspaces"]["workspace_names"]["12"], "Keep");
        assert_eq!(
            saved["workspaces"]["workspace_wallpapers"]["12"],
            "missing-legacy.exr"
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(save_at(
            &path,
            WorkspaceChange {
                expected,
                requested: requested.clone()
            }
        )
        .is_err());
        let mut invalid = requested.clone();
        invalid.workspace_names.remove(&12);
        assert!(save_at(
            &path,
            WorkspaceChange {
                expected: requested.clone(),
                requested: invalid
            }
        )
        .is_err());
        let mut invalid = requested.clone();
        invalid
            .workspace_wallpapers
            .insert(1, "https://example.com/wallpaper.png".into());
        assert!(save_at(
            &path,
            WorkspaceChange {
                expected: requested,
                requested: invalid
            }
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }
}
