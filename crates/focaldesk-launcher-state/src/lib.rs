use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LauncherState {
    pub favorites: Vec<String>,
    pub file_favorites: Vec<String>,
    pub recents: Vec<String>,
}

pub const MAX_RECENTS: usize = 12;

fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("launcher-state")
}

pub fn load_launcher_state() -> LauncherState {
    load_launcher_state_from(&state_path()).unwrap_or_default()
}

fn remember_recent(state: &mut LauncherState, id: &str) {
    state.recents.retain(|entry| entry != id);
    state.recents.insert(0, id.to_string());
    state.recents.truncate(MAX_RECENTS);
}

fn toggle_favorite(state: &mut LauncherState, id: &str) -> bool {
    toggle_entry(&mut state.favorites, id)
}

pub fn remember_recent_app(id: &str) -> io::Result<()> {
    mutate_launcher_state(&state_path(), |state| remember_recent(state, id))
}

pub fn toggle_app_favorite(id: &str) -> io::Result<bool> {
    mutate_launcher_state(&state_path(), |state| toggle_favorite(state, id))
}

pub fn toggle_file_favorite(uri: &str) -> io::Result<bool> {
    mutate_launcher_state(&state_path(), |state| {
        toggle_entry(&mut state.file_favorites, uri)
    })
}

pub fn remove_file_favorite(uri: &str) -> io::Result<bool> {
    mutate_launcher_state(&state_path(), |state| {
        let original_len = state.file_favorites.len();
        state.file_favorites.retain(|entry| entry != uri);
        state.file_favorites.len() != original_len
    })
}

pub fn is_file_favorite(uri: &str) -> bool {
    load_launcher_state()
        .file_favorites
        .iter()
        .any(|entry| entry == uri)
}

fn toggle_entry(entries: &mut Vec<String>, value: &str) -> bool {
    if let Some(index) = entries.iter().position(|entry| entry == value) {
        entries.remove(index);
        false
    } else {
        entries.push(value.to_string());
        true
    }
}

fn load_launcher_state_from(path: &Path) -> io::Result<LauncherState> {
    let contents = fs::read_to_string(path)?;
    let mut state = LauncherState::default();
    for line in contents.lines() {
        let (kind, value) = line.split_once('\t').unwrap_or((line, ""));
        if value.is_empty() {
            continue;
        }
        let entries = match kind {
            "favorite" => &mut state.favorites,
            "file-favorite" => &mut state.file_favorites,
            "recent" => &mut state.recents,
            _ => continue,
        };
        if !entries.iter().any(|entry| entry == value) {
            entries.push(value.to_string());
        }
    }
    state.recents.truncate(MAX_RECENTS);
    Ok(state)
}

fn mutate_launcher_state<T>(
    path: &Path,
    mutate: impl FnOnce(&mut LauncherState) -> T,
) -> io::Result<T> {
    with_state_lock(path, || {
        let mut state = match load_launcher_state_from(path) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => LauncherState::default(),
            Err(error) => return Err(error),
        };
        let result = mutate(&mut state);
        save_launcher_state_to(path, &state)?;
        Ok(result)
    })
}

fn with_state_lock<T>(path: &Path, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let parent = prepare_parent(path)?;
    let lock_path = parent.join("launcher-state.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .mode(0o600)
        .open(lock_path)?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;

    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    operation()
}

fn prepare_parent(path: &Path) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "launcher state path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(parent)
}

fn save_launcher_state_to(path: &Path, state: &LauncherState) -> io::Result<()> {
    let parent = prepare_parent(path)?;

    let mut contents = String::new();
    append_entries(&mut contents, "favorite", &state.favorites);
    append_entries(&mut contents, "file-favorite", &state.file_favorites);
    append_entries(&mut contents, "recent", &state.recents);
    write_private_atomic(parent, path, contents.as_bytes())
}

fn write_private_atomic(parent: &Path, path: &Path, contents: &[u8]) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("launcher-state");
    let mut temporary = None;
    for attempt in 0..100 {
        let candidate = parent.join(format!(
            ".{name}.{}.{stamp}.{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a launcher state temporary file",
        )
    })?;

    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary_path, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    result
}

fn append_entries(contents: &mut String, kind: &str, entries: &[String]) {
    for entry in entries {
        if entry.is_empty() || entry.contains(['\t', '\n', '\r']) {
            continue;
        }
        contents.push_str(kind);
        contents.push('\t');
        contents.push_str(entry);
        contents.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_launcher_state_from, mutate_launcher_state, remember_recent, save_launcher_state_to,
        toggle_favorite, LauncherState,
    };
    use std::os::unix::fs::PermissionsExt;

    fn test_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "focaldesk-launcher-state-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn round_trip_preserves_app_file_and_recent_entries() {
        let directory = test_directory("round-trip");
        let path = directory.join("launcher-state");
        let state = LauncherState {
            favorites: vec!["one.desktop".into()],
            file_favorites: vec!["file:///home/user/Notes.txt".into()],
            recents: vec!["two.desktop".into()],
        };

        save_launcher_state_to(&path, &state).expect("save state");
        assert_eq!(load_launcher_state_from(&path).expect("load state"), state);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_state_still_loads() {
        let directory = test_directory("legacy");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("launcher-state");
        std::fs::write(&path, "favorite\tone.desktop\nrecent\ttwo.desktop\n")
            .expect("write legacy state");

        let state = load_launcher_state_from(&path).expect("load legacy state");
        assert_eq!(state.favorites, ["one.desktop"]);
        assert_eq!(state.recents, ["two.desktop"]);
        assert!(state.file_favorites.is_empty());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn malformed_and_duplicate_entries_are_ignored() {
        let directory = test_directory("malformed");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("launcher-state");
        std::fs::write(
            &path,
            "garbage\nfavorite\tone.desktop\nfavorite\tone.desktop\nrecent\t\nunknown\tvalue\n",
        )
        .unwrap();

        let state = load_launcher_state_from(&path).unwrap();
        assert_eq!(state.favorites, ["one.desktop"]);
        assert!(state.file_favorites.is_empty());
        assert!(state.recents.is_empty());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn favorites_and_recents_are_deduplicated() {
        let mut state = LauncherState::default();
        assert!(toggle_favorite(&mut state, "one.desktop"));
        assert!(!toggle_favorite(&mut state, "one.desktop"));
        assert!(state.favorites.is_empty());

        remember_recent(&mut state, "one.desktop");
        remember_recent(&mut state, "two.desktop");
        remember_recent(&mut state, "one.desktop");
        assert_eq!(state.recents, ["one.desktop", "two.desktop"]);
    }

    #[test]
    fn concurrent_mutations_do_not_lose_entries() {
        let directory = test_directory("concurrent");
        let path = directory.join("launcher-state");
        let workers: Vec<_> = (0..16)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    mutate_launcher_state(&path, |state| {
                        state
                            .file_favorites
                            .push(format!("file:///favorite-{index}"));
                    })
                    .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }

        let state = load_launcher_state_from(&path).unwrap();
        assert_eq!(state.file_favorites.len(), 16);
        for index in 0..16 {
            assert!(state
                .file_favorites
                .contains(&format!("file:///favorite-{index}")));
        }
        let _ = std::fs::remove_dir_all(directory);
    }
}
