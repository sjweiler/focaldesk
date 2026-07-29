use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CameraStatus {
    pub detected: bool,
    pub active: bool,
}

/// Detect V4L2 camera nodes and whether a process visible to this user has one
/// open. An open node means "in use"; it does not prove that frames are being
/// recorded or retained.
pub fn camera_status() -> CameraStatus {
    let devices = camera_device_paths();
    CameraStatus {
        detected: !devices.is_empty(),
        active: !devices.is_empty() && any_process_has_device_open(&devices),
    }
}

fn camera_device_paths() -> HashSet<PathBuf> {
    let mut devices = HashSet::new();

    if let Ok(entries) = fs::read_dir("/sys/class/video4linux") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("video") {
                let path = Path::new("/dev").join(name);
                if path.exists() {
                    devices.insert(path);
                }
            }
        }
    }

    if devices.is_empty() {
        if let Ok(entries) = fs::read_dir("/dev") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("video") {
                    devices.insert(entry.path());
                }
            }
        }
    }

    devices
}

fn any_process_has_device_open(devices: &HashSet<PathBuf>) -> bool {
    let Ok(processes) = fs::read_dir("/proc") else {
        return false;
    };

    processes.flatten().any(|process| {
        if !process
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            return false;
        }

        fs::read_dir(process.path().join("fd"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|fd| fs::read_link(fd.path()).ok())
            .any(|target| devices.contains(&target))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_not_detected_or_active() {
        assert_eq!(
            CameraStatus::default(),
            CameraStatus {
                detected: false,
                active: false
            }
        );
    }
}
