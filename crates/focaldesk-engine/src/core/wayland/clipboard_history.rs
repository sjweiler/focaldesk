use std::collections::VecDeque;
use std::fs;
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
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }
}
