//! Compare-and-save input preferences without rewriting unrelated settings.
use crate::{InputSettings, default_settings, settings_path};
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn save_input_settings(expected: &InputSettings, input: &InputSettings) -> io::Result<()> {
    save_at(&settings_path(), expected, input)
}
fn merge_known(target: &mut Value, updates: Value) {
    match (target, updates) {
        (Value::Object(target), Value::Object(updates)) => {
            for (key, value) in updates {
                merge_known(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, updates) => *target = updates,
    }
}
fn save_at(path: &Path, expected: &InputSettings, input: &InputSettings) -> io::Result<()> {
    let mut root: Value = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error),
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| io::Error::other("settings.json must be an object"))?;
    let current = object
        .get("input")
        .map(|input| {
            serde_json::from_value::<InputSettings>(input.clone()).map_err(io::Error::other)
        })
        .transpose()?
        .unwrap_or_else(|| default_settings().input);
    if current != *expected {
        return Err(io::Error::other(
            "Saved input settings changed in another client. Reload before applying.",
        ));
    }
    let updates = serde_json::to_value(input).map_err(io::Error::other)?;
    let target = object.entry("input").or_insert(Value::Null);
    // Explicitly remove deleted known shortcut overrides; recursive merging
    // otherwise retains future fields in input/accessibility/device objects.
    if let Some(target) = target.as_object_mut() {
        target.insert("keybindings".into(), serde_json::json!(input.keybindings));
    }
    merge_known(target, updates);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing settings directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".input-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&root).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_other_settings_future_fields_and_rejects_concurrent_edits() {
        let root = std::env::temp_dir().join(format!(
            "focaldesk-input-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        let expected = default_settings().input;
        let mut document = serde_json::json!({"input":expected,"displays":{"outputs":[]},"desktop_config":{"appearance":{"theme":"Moonbase"}},"unknown":42});
        document["input"]["future_input"] = true.into();
        document["input"]["accessibility"]["future_accessibility"] = 17.into();
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let mut requested = expected.clone();
        requested.keyboard_repeat_rate = 40;
        requested
            .keybindings
            .insert("launch_terminal".into(), "Ctrl+Alt+T".into());
        save_at(&path, &expected, &requested).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["desktop_config"], document["desktop_config"]);
        assert_eq!(saved["displays"], document["displays"]);
        assert_eq!(saved["unknown"], 42);
        assert_eq!(saved["input"]["future_input"], true);
        assert_eq!(saved["input"]["accessibility"]["future_accessibility"], 17);
        let before = fs::read(&path).unwrap();
        assert!(save_at(&path, &expected, &requested).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut reset = requested.clone();
        reset.keybindings.clear();
        save_at(&path, &requested, &reset).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["input"]["keybindings"], serde_json::json!({}));
        fs::write(&path, "invalid").unwrap();
        assert!(save_at(&path, &reset, &reset).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid");
        fs::remove_dir_all(root).unwrap();
    }
}
