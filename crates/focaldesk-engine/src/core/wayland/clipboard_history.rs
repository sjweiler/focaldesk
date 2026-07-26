use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: u64,
    pub mime_type: String,
    pub text: String,
    pub timestamp_secs: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipboardEntry>,
    next_id: u64,
}

impl ClipboardHistory {
    /// Add a new entry unless its text is identical to the most recent one.
    pub fn push(&mut self, mime_type: String, text: String) -> u64 {
        if let Some(front) = self.entries.front() {
            if front.text == text {
                return front.id;
            }
        }

        let id = self.next_id;
        self.next_id += 1;

        self.entries.push_front(ClipboardEntry {
            id,
            mime_type,
            text,
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });

        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_back();
        }

        self.save();

        id
    }

    pub fn entries(&self) -> impl Iterator<Item = &ClipboardEntry> {
        self.entries.iter()
    }

    pub fn get(&self, id: u64) -> Option<&ClipboardEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn remove(&mut self, id: u64) {
        self.entries.retain(|entry| entry.id != id);
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    fn path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|dir| dir.join("focaldesk").join("clipboard_history.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
            if fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).is_err() {
                return;
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = write_private_atomic(&path, json.as_bytes());
        }
    }
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "clipboard history path has no parent",
        )
    })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(
        ".clipboard-history-{}-{stamp}.tmp",
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;

    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_atomic_write_uses_owner_only_permissions() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "focaldesk-clipboard-permissions-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("clipboard_history.json");

        write_private_atomic(&path, b"{}").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(directory);
    }
}
