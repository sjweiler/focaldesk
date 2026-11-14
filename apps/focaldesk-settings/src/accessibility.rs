//! Accessibility preference transaction, owned and validated by Rust.
use crate::preferences::{read_document, reload_message, write_document};
use anyhow::{bail, Result};
use focaldesk_settings_core::{AccessibilitySettings, KeyboardAccessibilitySettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityDraft {
    pub visual: AccessibilitySettings,
    pub keyboard: KeyboardAccessibilitySettings,
}

#[derive(Serialize)]
pub struct AccessibilitySnapshot {
    pub preferences: AccessibilityDraft,
    pub outputs: Vec<String>,
}

#[derive(Serialize)]
pub struct AccessibilityResult {
    pub snapshot: AccessibilitySnapshot,
    pub message: String,
}

fn from_document(document: &Value) -> Result<AccessibilitySnapshot> {
    let visual = document
        .get("accessibility")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if document
        .get("input")
        .is_some_and(|input| !input.is_object())
    {
        bail!("The input settings section is malformed.");
    }
    let keyboard = document
        .pointer("/input/accessibility")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let outputs = document
        .pointer("/displays/outputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|output| {
            output
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .filter_map(|output| {
            output
                .get("connector")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    Ok(AccessibilitySnapshot {
        preferences: AccessibilityDraft { visual, keyboard },
        outputs,
    })
}

pub fn snapshot() -> Result<AccessibilitySnapshot> {
    from_document(&read_document(&focaldesk_settings_core::settings_path())?)
}

fn validate(requested: &AccessibilityDraft) -> Result<()> {
    if requested.visual != requested.visual.clone().sanitized() {
        bail!("Visual accessibility values are outside their supported ranges. Check magnification, pointer size, temperature, schedule, coordinates, and brightness.");
    }
    if requested.keyboard != requested.keyboard.sanitized() {
        bail!("Slow Keys and Bounce Keys delays must be between 100 and 2000 milliseconds.");
    }
    if requested.visual.magnifier_output_zoom.len() > 256
        || requested.visual.display_comfort_disabled_outputs.len() > 256
        || requested
            .visual
            .magnifier_output_zoom
            .keys()
            .chain(requested.visual.display_comfort_disabled_outputs.keys())
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(char::is_control))
    {
        bail!("Invalid display connector preferences.");
    }
    Ok(())
}

fn merge_known(target: &mut Value, updates: Value) {
    match (target, updates) {
        (Value::Object(target), Value::Object(updates)) => {
            for (key, value) in updates {
                // These maps are user-editable collections, so deletion is meaningful.
                if matches!(
                    key.as_str(),
                    "magnifier_output_zoom" | "display_comfort_disabled_outputs"
                ) {
                    target.insert(key, value);
                } else {
                    merge_known(target.entry(key).or_insert(Value::Null), value);
                }
            }
        }
        (target, updates) => *target = updates,
    }
}

fn save_at(
    path: &Path,
    expected: &AccessibilityDraft,
    requested: &AccessibilityDraft,
) -> Result<AccessibilitySnapshot> {
    validate(requested)?;
    let mut document = read_document(path)?;
    if from_document(&document)?.preferences != *expected {
        bail!("Accessibility preferences changed in another client. Reload before applying.");
    }
    let root = document.as_object_mut().unwrap();
    merge_known(
        root.entry("accessibility").or_insert(Value::Null),
        serde_json::to_value(&requested.visual)?,
    );
    let input = root
        .entry("input")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .unwrap();
    merge_known(
        input.entry("accessibility").or_insert(Value::Null),
        serde_json::to_value(requested.keyboard)?,
    );
    write_document(path, &document)?;
    from_document(&document)
}

pub fn apply(
    expected: AccessibilityDraft,
    requested: AccessibilityDraft,
) -> Result<AccessibilityResult> {
    let snapshot = save_at(
        &focaldesk_settings_core::settings_path(),
        &expected,
        &requested,
    )?;
    Ok(AccessibilityResult {
        snapshot,
        message: reload_message(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn defaults() -> AccessibilityDraft {
        AccessibilityDraft {
            visual: AccessibilitySettings::default(),
            keyboard: KeyboardAccessibilitySettings::default(),
        }
    }
    #[test]
    fn rejects_invalid_ranges_and_nonfinite_values_without_clamping() {
        let mut draft = defaults();
        assert!(validate(&draft).is_ok());
        draft.visual.magnifier_zoom = f32::NAN;
        assert!(validate(&draft).is_err());
        draft = defaults();
        draft.visual.night_light.latitude = Some(91.0);
        assert!(validate(&draft).is_err());
        draft = defaults();
        draft.visual.night_light.custom_start_minutes = 1440;
        assert!(validate(&draft).is_err());
        draft = defaults();
        draft.keyboard.slow_keys_delay_ms = 0;
        assert!(validate(&draft).is_err());
        draft = defaults();
        draft.visual.cursor_size = 100;
        assert!(validate(&draft).is_err());
    }
    #[test]
    fn transaction_preserves_other_preferences_future_fields_and_rejects_stale_changes() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-accessibility-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        let initial = serde_json::json!({"accessibility":{"future":1,"night_light":{"future":2},"magnifier_output_zoom":{"DP-1":3.0}},"input":{"keyboard_layout":"de","keybindings":{"launch_terminal":"Ctrl+Alt+T"},"accessibility":{"future":3}},"privacy":{"recent_files":false},"displays":{"outputs":[]}});
        std::fs::write(&path, initial.to_string()).unwrap();
        let expected = from_document(&initial).unwrap().preferences;
        let mut requested = expected.clone();
        requested.keyboard.sticky_keys = true;
        requested.visual.cursor_size = 48;
        requested.visual.magnifier_output_zoom.clear();
        save_at(&path, &expected, &requested).unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(saved["input"]["keyboard_layout"], "de");
        assert_eq!(
            saved["input"]["keybindings"],
            initial["input"]["keybindings"]
        );
        assert_eq!(saved["privacy"], initial["privacy"]);
        assert_eq!(saved["displays"], initial["displays"]);
        assert_eq!(saved["accessibility"]["future"], 1);
        assert_eq!(saved["accessibility"]["night_light"]["future"], 2);
        assert_eq!(saved["input"]["accessibility"]["future"], 3);
        assert_eq!(
            saved["accessibility"]["magnifier_output_zoom"],
            serde_json::json!({})
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(save_at(&path, &expected, &requested).is_err());
        assert_eq!(bytes, std::fs::read(&path).unwrap());
        std::fs::write(&path, "broken").unwrap();
        assert!(save_at(&path, &expected, &requested).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_dir_all(root).unwrap();
    }
}
