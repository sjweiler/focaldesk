use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::Uid;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
pub const IPC_PROTOCOL_VERSION: u16 = 1;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub pid: i32,
    pub executable: PathBuf,
    pub unit: Option<String>,
    executable_trusted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PeerPolicy<'a> {
    pub endpoint: &'static str,
    pub allowed_executables: &'a [&'a str],
    pub allowed_units: &'a [&'a str],
}

pub const DESKTOP_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "desktop",
    allowed_executables: &[
        "focal-dock",
        "focal-panel",
        "focaldesk-settings",
        "focaldesk-portal",
        "focaldesk-cli",
        "focaldesk-ai-console",
        "focaldesk-mcp",
        "focald-voice",
    ],
    allowed_units: &[
        "focaldesk-dock.service",
        "focaldesk-panel.service",
        "focald-voice.service",
    ],
};

pub const SETTINGS_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "settings",
    allowed_executables: &["focaldesk-settings"],
    allowed_units: &[],
};

pub const CONTROL_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "controls",
    allowed_executables: &["focaldesk-desktop", "focaldesk-automation"],
    allowed_units: &["focaldesk-automation.service", "focaldesk-session.target"],
};

pub const DIALOG_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "dialog",
    allowed_executables: &["focaldesk-polkitd", "focaldesk-portal", "focaldesk-server"],
    allowed_units: &["focaldesk-polkitd.service", "focaldesk-server.service"],
};

pub const NOTIFICATIONS_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "notifications",
    allowed_executables: &[
        "focaldesk-desktop",
        "focaldesk-automation",
        "focaldesk-cli",
        "focaldesk-ai-console",
        "focaldesk-mcp",
    ],
    allowed_units: &["focaldesk-automation.service"],
};

pub const POWER_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "power",
    allowed_executables: &["focaldesk-desktop", "focaldesk-settings"],
    allowed_units: &[],
};

pub const AI_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "ai",
    allowed_executables: &["focaldesk-desktop", "focaldesk-cli", "focaldesk-ai-console"],
    allowed_units: &[],
};

pub const AUTOMATION_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "automation",
    allowed_executables: &["focaldesk-cli", "focaldesk-settings"],
    allowed_units: &[],
};

pub const LAUNCH_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "launch",
    allowed_executables: &["focaldesk-desktop"],
    allowed_units: &[],
};

pub const MIC_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "microphone",
    allowed_executables: &["focaldesk-desktop", "focald-mic"],
    allowed_units: &["focald-mic.service"],
};

pub const VOICE_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "voice",
    allowed_executables: &["focald-mic"],
    allowed_units: &["focald-mic.service"],
};

pub const SPEECH_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "speech",
    allowed_executables: &["focald-mic", "focald-speech"],
    allowed_units: &["focald-mic.service", "focald-speech.service"],
};

#[derive(Debug, Serialize, Deserialize)]
struct WireEnvelope<T> {
    protocol_version: u16,
    payload: T,
}

pub fn encode_message<T: Serialize>(payload: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&WireEnvelope {
        protocol_version: IPC_PROTOCOL_VERSION,
        payload,
    })
    .map_err(|err| err.to_string())
}

pub fn decode_message<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let envelope: WireEnvelope<T> =
        serde_json::from_slice(bytes).map_err(|err| format!("invalid IPC envelope: {err}"))?;
    if envelope.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(format!(
            "unsupported IPC protocol version {}; supported version is {}",
            envelope.protocol_version, IPC_PROTOCOL_VERSION
        ));
    }
    Ok(envelope.payload)
}

/// Resolve an IPC endpoint below the private per-user runtime directory.
///
/// Explicit paths remain available for development and isolated tests. Normal
/// sessions must provide XDG_RUNTIME_DIR; falling back to the shared /tmp
/// namespace would recreate the cross-user boundary this transport protects.
pub fn socket_path(env_name: &str, socket_name: &str) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(env_name).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("XDG_RUNTIME_DIR is not set; cannot resolve {socket_name}"))?;
    Ok(PathBuf::from(runtime).join("focaldesk").join(socket_name))
}

/// Bind a socket that is private to the current user.
pub fn bind_user_socket(path: &Path) -> io::Result<UnixListener> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC socket has no parent directory",
        )
    })?;
    prepare_private_directory(parent)?;
    remove_stale_socket(path)?;

    let listener = UnixListener::bind(path)?;
    if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        let _ = fs::remove_file(path);
        return Err(err);
    }
    Ok(listener)
}

/// Reject clients from a different effective user, even if socket permissions
/// are accidentally broadened by packaging or an administrator.
pub fn require_same_user(stream: &impl AsFd) -> io::Result<()> {
    peer_identity(stream).map(|_| ())
}

pub fn require_authorized_peer(
    stream: &impl AsFd,
    policy: PeerPolicy<'_>,
) -> io::Result<PeerIdentity> {
    let identity = peer_identity(stream).map_err(|err| {
        tracing::warn!(
            target: "focaldesk.ipc",
            endpoint = policy.endpoint,
            error = %err,
            "rejected unidentified IPC peer"
        );
        err
    })?;
    if policy_allows(&identity, policy) {
        return Ok(identity);
    }
    let err = io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} IPC denied peer pid={} exe={} unit={}",
            policy.endpoint,
            identity.pid,
            identity.executable.display(),
            identity.unit.as_deref().unwrap_or("none")
        ),
    );
    tracing::warn!(
        target: "focaldesk.ipc",
        endpoint = policy.endpoint,
        peer_pid = identity.pid,
        peer_executable = %identity.executable.display(),
        peer_unit = identity.unit.as_deref().unwrap_or("none"),
        "rejected unauthorized IPC peer"
    );
    Err(err)
}

pub fn peer_identity(stream: &impl AsFd) -> io::Result<PeerIdentity> {
    let credentials = getsockopt(stream, PeerCredentials).map_err(io::Error::other)?;
    let expected = Uid::effective().as_raw();
    if credentials.uid() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "IPC peer uid {} does not match service uid {expected}",
                credentials.uid()
            ),
        ));
    }
    let pid = credentials.pid();
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC peer did not provide a valid pid",
        ));
    }
    // Pin the kernel pid object while resolving /proc identity so the numeric
    // pid cannot be recycled between SO_PEERCRED and executable inspection.
    // SAFETY: pidfd_open returns a new owned descriptor or -1.
    let _pidfd = unsafe {
        let fd = libc::syscall(libc::SYS_pidfd_open, pid, 0_u32);
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("IPC peer pid {pid} exited before identification"),
            ));
        }
        OwnedFd::from_raw_fd(fd as i32)
    };
    let proc_executable = PathBuf::from(format!("/proc/{pid}/exe"));
    let executable = fs::read_link(&proc_executable).map_err(|err| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("could not identify IPC peer pid {pid}: {err}"),
        )
    })?;
    // Inspect the procfs executable link while the pidfd still pins this process.
    // Unlike the resolved path, /proc/<pid>/exe remains stat-able after an
    // atomic package upgrade unlinks the running executable.
    let executable_trusted = executable_path_is_trusted(&proc_executable);
    let unit = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|contents| systemd_unit_from_cgroup(&contents));
    Ok(PeerIdentity {
        uid: credentials.uid(),
        pid,
        executable,
        unit,
        executable_trusted,
    })
}

pub fn configure_stream(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))
}

pub fn read_limited(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    configure_stream(stream)?;
    read_limited_from(stream)
}

fn read_limited_from(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IPC request exceeds {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    Ok(bytes)
}

fn prepare_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "IPC runtime path {} is not a real directory",
                path.display()
            ),
        ));
    }
    let expected = Uid::effective().as_raw();
    if metadata.uid() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "IPC runtime directory {} is owned by uid {}, expected {expected}",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let expected = Uid::effective().as_raw();
    if !metadata.file_type().is_socket() || metadata.uid() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to replace non-socket or foreign-owned IPC path {}",
                path.display()
            ),
        ));
    }
    fs::remove_file(path)
}

fn policy_allows(identity: &PeerIdentity, policy: PeerPolicy<'_>) -> bool {
    let executable = identity
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        // Linux annotates a running executable whose directory entry was
        // replaced during an upgrade. The inode is still pinned and verified
        // above, so this suffix is state, not part of the executable name.
        .map(|name| name.strip_suffix(" (deleted)").unwrap_or(name));
    let executable_allowed = executable.is_some_and(|name| {
        policy.allowed_executables.contains(&name) && executable_identity_is_trusted(identity)
    });
    let unit_allowed = identity
        .unit
        .as_deref()
        .is_some_and(|unit| policy.allowed_units.contains(&unit));
    executable_allowed || unit_allowed
}

fn executable_identity_is_trusted(identity: &PeerIdentity) -> bool {
    identity.executable_trusted
}

fn executable_path_is_trusted(path: &Path) -> bool {
    if cfg!(test)
        || cfg!(debug_assertions)
        || std::env::var_os("FOCALDESK_ALLOW_USER_OWNED_IPC_PEERS").is_some()
    {
        return true;
    }

    fs::metadata(path)
        .map(|metadata| metadata.uid() == 0 && metadata.mode() & 0o022 == 0)
        .unwrap_or(false)
}

fn systemd_unit_from_cgroup(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let path = line.split_once("::")?.1;
        path.rsplit('/').find_map(|component| {
            (component.ends_with(".service") || component.ends_with(".scope"))
                .then(|| component.to_string())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_policy_allows_trusted_shell_clients() {
        assert!(DESKTOP_POLICY.allowed_executables.contains(&"focal-dock"));
        assert!(DESKTOP_POLICY.allowed_executables.contains(&"focal-panel"));
        assert!(
            DESKTOP_POLICY
                .allowed_units
                .contains(&"focaldesk-dock.service")
        );
        assert!(
            DESKTOP_POLICY
                .allowed_units
                .contains(&"focaldesk-panel.service")
        );
    }

    #[test]
    fn private_listener_uses_restrictive_permissions_and_accepts_same_user() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("focaldesk");
        let socket = runtime.join("test.sock");
        let listener = match bind_user_socket(&socket) {
            Ok(listener) => listener,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                // Some restricted test sandboxes prohibit AF_UNIX entirely.
                return;
            }
            Err(err) => panic!("bind private test socket: {err}"),
        };

        assert_eq!(
            fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let client = UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();
        require_same_user(&server).unwrap();
        let current_executable = std::env::current_exe().unwrap();
        let executable_name = current_executable.file_name().unwrap().to_str().unwrap();
        let allowed = [executable_name];
        require_authorized_peer(
            &server,
            PeerPolicy {
                endpoint: "test",
                allowed_executables: &allowed,
                allowed_units: &[],
            },
        )
        .unwrap();
        let denied = require_authorized_peer(
            &server,
            PeerPolicy {
                endpoint: "test",
                allowed_executables: &[],
                allowed_units: &[],
            },
        )
        .unwrap_err();
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        drop(client);
    }

    #[test]
    fn bind_refuses_to_replace_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("focaldesk");
        fs::create_dir(&runtime).unwrap();
        let socket = runtime.join("test.sock");
        fs::write(&socket, b"do not replace").unwrap();

        let err = bind_user_socket(&socket).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&socket).unwrap(), b"do not replace");
    }

    #[test]
    fn oversized_requests_are_rejected() {
        let bytes = vec![0_u8; MAX_REQUEST_BYTES as usize + 1];
        let err = read_limited_from(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn cgroup_identity_prefers_the_nearest_service_or_scope() {
        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/focaldesk-polkitd.service\n";
        assert_eq!(
            systemd_unit_from_cgroup(cgroup).as_deref(),
            Some("focaldesk-polkitd.service")
        );
    }

    #[test]
    fn endpoint_policy_is_deny_by_default() {
        let identity = PeerIdentity {
            uid: 1000,
            pid: 42,
            executable: PathBuf::from("/usr/bin/untrusted-app"),
            unit: Some("app-untrusted.scope".to_string()),
            executable_trusted: false,
        };
        let policy = PeerPolicy {
            endpoint: "dialog",
            allowed_executables: &["focaldesk-polkitd"],
            allowed_units: &["focaldesk-polkitd.service"],
        };
        assert!(!policy_allows(&identity, policy));
    }

    #[test]
    fn endpoint_policy_accepts_an_allowed_executable_or_unit() {
        let by_executable = PeerIdentity {
            uid: 1000,
            pid: 42,
            executable: PathBuf::from("/usr/bin/focaldesk-polkitd"),
            unit: None,
            executable_trusted: true,
        };
        let by_unit = PeerIdentity {
            uid: 1000,
            pid: 43,
            executable: PathBuf::from("/usr/bin/launcher-wrapper"),
            unit: Some("focaldesk-polkitd.service".to_string()),
            executable_trusted: false,
        };
        let policy = PeerPolicy {
            endpoint: "dialog",
            allowed_executables: &["focaldesk-polkitd"],
            allowed_units: &["focaldesk-polkitd.service"],
        };
        assert!(policy_allows(&by_executable, policy));
        assert!(policy_allows(&by_unit, policy));
    }

    #[test]
    fn endpoint_policy_accepts_a_trusted_executable_replaced_during_upgrade() {
        let policy = PeerPolicy {
            endpoint: "ai",
            allowed_executables: &["focaldesk-ai-console"],
            allowed_units: &[],
        };
        let trusted = PeerIdentity {
            uid: 1000,
            pid: 42,
            executable: PathBuf::from("/usr/local/bin/focaldesk-ai-console (deleted)"),
            unit: Some("focal-launchd.service".to_string()),
            executable_trusted: true,
        };
        let untrusted = PeerIdentity {
            executable_trusted: false,
            ..trusted.clone()
        };

        assert!(policy_allows(&trusted, policy));
        assert!(!policy_allows(&untrusted, policy));
    }

    #[test]
    fn protocol_envelope_round_trips_and_rejects_other_versions() {
        let encoded = encode_message(&"ping").unwrap();
        assert_eq!(decode_message::<String>(&encoded).unwrap(), "ping");

        let unsupported = br#"{"protocol_version":99,"payload":"ping"}"#;
        let err = decode_message::<String>(unsupported).unwrap_err();
        assert!(err.contains("unsupported IPC protocol version 99"));
    }

    #[test]
    fn legacy_unversioned_messages_are_rejected_cleanly() {
        let err = decode_message::<serde_json::Value>(br#"{"type":"status"}"#).unwrap_err();
        assert!(err.contains("invalid IPC envelope"));
    }
}
