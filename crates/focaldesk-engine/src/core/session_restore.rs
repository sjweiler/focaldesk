use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SNAPSHOT_VERSION: u32 = 1;
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
const STARTUP_RESTORE_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedProtocol {
    Wayland,
    Xwayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub protocol: SavedProtocol,
    /// Wayland app_id or X11 WM_CLASS. It is used both to find an installed
    /// desktop entry and to associate the new window with this record.
    pub app_identity: String,
    pub instance: u32,
    pub workspace: String,
    pub output_connector: Option<String>,
    /// Logical geometry relative to the saved output's origin. This survives
    /// monitor rearrangement and scale changes better than global coordinates.
    pub geometry: SavedRect,
    pub restore_geometry: Option<SavedRect>,
    pub floating: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedOutput {
    pub connector: String,
    pub active_workspace: String,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<SavedOutput>,
    pub windows: Vec<SavedWindow>,
}

impl SessionSnapshot {
    pub fn new(
        workspaces: Vec<String>,
        outputs: Vec<SavedOutput>,
        windows: Vec<SavedWindow>,
    ) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            workspaces,
            outputs,
            windows,
        }
    }
}

pub struct SessionRestoreState {
    path: PathBuf,
    enabled: bool,
    pending: Vec<SavedWindow>,
    pending_outputs: Vec<SavedOutput>,
    next_checkpoint: Instant,
    restore_grace_until: Option<Instant>,
    last_contents: Option<Vec<u8>>,
}

impl SessionRestoreState {
    pub fn load_default(enabled: bool) -> (Self, Option<SessionSnapshot>) {
        Self::load(session_state_path(), enabled)
    }

    fn load(path: PathBuf, enabled: bool) -> (Self, Option<SessionSnapshot>) {
        if !enabled {
            let _ = fs::remove_file(&path);
        }
        let snapshot = enabled.then(|| load_snapshot(&path)).flatten();
        let pending = snapshot
            .as_ref()
            .map(|snapshot| snapshot.windows.clone())
            .unwrap_or_default();
        let pending_outputs = snapshot
            .as_ref()
            .map(|snapshot| snapshot.outputs.clone())
            .unwrap_or_default();
        let now = Instant::now();
        let restore_grace_until = (!pending.is_empty()).then(|| now + STARTUP_RESTORE_GRACE);
        let state = Self {
            path,
            enabled,
            pending,
            pending_outputs,
            next_checkpoint: now + CHECKPOINT_INTERVAL,
            restore_grace_until,
            last_contents: None,
        };
        (state, snapshot)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.next_checkpoint = Instant::now();
        if !enabled {
            self.pending.clear();
            self.pending_outputs.clear();
            self.restore_grace_until = None;
            self.last_contents = None;
            match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => focaldesk_logging::flog_warn!(
                    "Failed to remove disabled session snapshot {}: {e}",
                    self.path.display()
                ),
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn due(&self, now: Instant) -> bool {
        self.enabled
            && now >= self.next_checkpoint
            && self
                .restore_grace_until
                .is_none_or(|deadline| self.pending.is_empty() || now >= deadline)
    }

    pub fn save(&mut self, snapshot: &SessionSnapshot, force: bool) -> io::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        let contents = serde_json::to_vec_pretty(snapshot)?;
        if !force && self.last_contents.as_deref() == Some(contents.as_slice()) {
            self.next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;
            return Ok(false);
        }
        atomic_write(&self.path, &contents)?;
        self.last_contents = Some(contents);
        self.next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;
        Ok(true)
    }

    pub fn take_match(&mut self, protocol: SavedProtocol, identity: &str) -> Option<SavedWindow> {
        let identity = normalize_identity(identity);
        let index = self.pending.iter().position(|saved| {
            saved.protocol == protocol && normalize_identity(&saved.app_identity) == identity
        })?;
        let saved = self.pending.remove(index);
        if self.pending.is_empty() {
            self.restore_grace_until = None;
            self.next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;
        }
        Some(saved)
    }

    pub fn take_output(&mut self, connector: &str) -> Option<SavedOutput> {
        let index = self
            .pending_outputs
            .iter()
            .position(|output| output.connector == connector)?;
        Some(self.pending_outputs.remove(index))
    }
}

pub fn session_state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("session.json")
}

pub fn desktop_entry_for_identity(identity: &str) -> Option<PathBuf> {
    let trimmed = identity.trim();
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('\0') {
        return None;
    }
    let filename = if trimmed.ends_with(".desktop") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.desktop")
    };

    let directories = application_dirs();
    for directory in &directories {
        for candidate_name in [&filename, &filename.to_ascii_lowercase()] {
            let candidate = directory.join(candidate_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // XWayland commonly exposes StartupWMClass rather than the desktop file
    // basename. Scan only the standard application directories and accept an
    // exact, case-insensitive StartupWMClass match.
    let wanted = normalize_identity(trimmed);
    directories.into_iter().find_map(|directory| {
        fs::read_dir(directory)
            .ok()?
            .flatten()
            .take(4096)
            .find_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                    return None;
                }
                if fs::metadata(&path).ok()?.len() > 256 * 1024 {
                    return None;
                }
                let contents = fs::read_to_string(&path).ok()?;
                contents.lines().find_map(|line| {
                    line.strip_prefix("StartupWMClass=")
                        .is_some_and(|value| normalize_identity(value) == wanted)
                        .then(|| path.clone())
                })
            })
    })
}

fn application_dirs() -> Vec<PathBuf> {
    let mut dirs_out = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs_out.push(data_home.join("applications"));
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs_out.push(home.join(".local/share/applications"));
    }

    let data_dirs =
        std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs_out.extend(std::env::split_paths(&data_dirs).map(|dir| dir.join("applications")));
    dirs_out
}

fn normalize_identity(identity: &str) -> String {
    identity
        .trim()
        .trim_end_matches(".desktop")
        .to_ascii_lowercase()
}

fn load_snapshot(path: &Path) -> Option<SessionSnapshot> {
    let contents = fs::read(path).ok()?;
    if contents.len() > 1024 * 1024 {
        return None;
    }
    let snapshot: SessionSnapshot = serde_json::from_slice(&contents).ok()?;
    (snapshot.version == SNAPSHOT_VERSION).then_some(snapshot)
}

fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(identity: &str, instance: u32) -> SavedWindow {
        SavedWindow {
            protocol: SavedProtocol::Wayland,
            app_identity: identity.into(),
            instance,
            workspace: "Work".into(),
            output_connector: Some("DP-1".into()),
            geometry: SavedRect {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
            },
            restore_geometry: None,
            floating: true,
            maximized: false,
            fullscreen: false,
            minimized: false,
            focused: false,
        }
    }

    #[test]
    fn snapshot_round_trips_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "focaldesk-session-restore-test-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let path = dir.join("session.json");
        let snapshot = SessionSnapshot::new(
            vec!["Work".into()],
            vec![SavedOutput {
                connector: "DP-1".into(),
                active_workspace: "Work".into(),
                focused: true,
            }],
            vec![saved("org.test.App", 0)],
        );
        atomic_write(&path, &serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();
        assert_eq!(load_snapshot(&path), Some(snapshot));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn matching_is_case_and_desktop_suffix_insensitive() {
        let dir = std::env::temp_dir().join(format!(
            "focaldesk-session-match-test-{}",
            std::process::id()
        ));
        let (mut state, _) = SessionRestoreState::load(dir.join("missing"), false);
        state.pending = vec![saved("org.test.App.desktop", 0), saved("org.test.App", 1)];
        assert_eq!(
            state
                .take_match(SavedProtocol::Wayland, "ORG.TEST.APP")
                .unwrap()
                .instance,
            0
        );
        assert_eq!(
            state
                .take_match(SavedProtocol::Wayland, "org.test.App")
                .unwrap()
                .instance,
            1
        );
    }

    #[test]
    fn rejects_untrusted_desktop_entry_paths() {
        assert_eq!(desktop_entry_for_identity("../../evil"), None);
        assert_eq!(desktop_entry_for_identity(""), None);
    }

    #[test]
    fn loading_with_restore_disabled_removes_an_old_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "focaldesk-session-disabled-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        fs::write(&path, b"old state").unwrap();
        let (_state, snapshot) = SessionRestoreState::load(path.clone(), false);
        assert_eq!(snapshot, None);
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
