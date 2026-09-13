use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 50;
const RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

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
        self.prune_at(now_secs());
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
            timestamp_secs: now_secs(),
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
        let mut history = match fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        if history.prune_at(now_secs()) {
            history.save();
        }
        history
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

    fn prune_at(&mut self, now_secs: u64) -> bool {
        let original_len = self.entries.len();
        self.entries.retain(|entry| {
            now_secs.saturating_sub(entry.timestamp_secs) <= RETENTION_SECS
                && entry.timestamp_secs <= now_secs.saturating_add(24 * 60 * 60)
        });
        self.entries.len() != original_len
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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

    #[test]
    fn clipboard_history_prunes_entries_older_than_thirty_days() {
        let now = 1_800_000_000;
        let mut history = ClipboardHistory {
            entries: VecDeque::from([
                ClipboardEntry {
                    id: 2,
                    mime_type: "text/plain".into(),
                    text: "current".into(),
                    timestamp_secs: now - RETENTION_SECS,
                },
                ClipboardEntry {
                    id: 1,
                    mime_type: "text/plain".into(),
                    text: "expired".into(),
                    timestamp_secs: now - RETENTION_SECS - 1,
                },
            ]),
            next_id: 3,
        };

        assert!(history.prune_at(now));
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].text, "current");
    }
}
