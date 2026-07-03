//! Spawn helper for the compositor.
//!
//! The desktop must not `fork()` after GLES/EGL init (GPU drivers deadlock). A tiny
//! daemon is forked once at compositor startup (before GL) and performs all app spawns.

mod apptarget;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::thread;
use std::time::Duration;

pub use apptarget::{AppTarget, SpawnRequest, Spawner};

fn spawn_trace(msg: impl AsRef<str>) {
    if std::env::var_os("FOCALDESK_SPAWN_TRACE").is_none() {
        return;
    }
    eprintln!(
        "[focaldesk-spawn pid={}] {}",
        std::process::id(),
        msg.as_ref()
    );
}

fn spawn_notice(msg: impl AsRef<str>) {
    eprintln!("[focaldesk-spawn pid={}] {}", std::process::id(), msg.as_ref());
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SpawnMessage {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub unset_env: Vec<String>,
    /// Remove stale Chromium singleton locks in this profile before spawning.
    #[serde(default)]
    pub clear_chrome_profile: Option<String>,
    /// Append child stdout/stderr to this path when set.
    #[serde(default)]
    pub log_path: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SpawnResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("focaldesk-spawn.sock")
}

fn daemon_reachable() -> bool {
    spawn_trace(format!("daemon_reachable? socket={}", socket_path().display()));
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    let _ = stream.write_all(b"{\"ping\":true}\n");
    spawn_trace("daemon reachable");
    true
}

fn clear_stale_chrome_singleton(profile: &std::path::Path) {
    let lock = profile.join("SingletonLock");
    let Ok(target) = std::fs::read_link(&lock) else {
        return;
    };

    let Some(pid) = target
        .to_string_lossy()
        .rsplit('-')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return;
    };

    if std::path::PathBuf::from(format!("/proc/{pid}")).exists() {
        return;
    }

    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let path = profile.join(name);
        let _ = std::fs::remove_file(path);
    }
}

fn run_daemon() -> std::io::Result<()> {
    let path = socket_path();
    spawn_trace(format!("starting daemon at {}", path.display()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    loop {
        let (mut stream, _) = listener.accept()?;
        thread::spawn(move || {
            spawn_trace("accepted connection");
            let mut payload = String::new();
            if stream.read_to_string(&mut payload).is_err() {
                spawn_trace("failed reading spawn payload");
                return;
            }
            let Some(line) = payload.lines().find(|line| !line.trim().is_empty()) else {
                spawn_trace("empty spawn payload");
                return;
            };
            if line.contains("\"ping\"") {
                spawn_trace("ping received");
                return;
            }
            let Ok(msg) = serde_json::from_str::<SpawnMessage>(line) else {
                spawn_trace("failed to decode spawn payload");
                return;
            };
            spawn_trace(format!(
                "spawning program={} args={}",
                msg.program,
                msg.args.len()
            ));
            if let Some(profile) = msg.clear_chrome_profile.as_deref() {
                clear_stale_chrome_singleton(std::path::Path::new(profile));
            }
            let mut response = SpawnResponse {
                ok: false,
                error: None,
            };
            let mut command = Command::new(&msg.program);
            command.args(&msg.args);
            for (key, value) in &msg.env {
                command.env(key, value);
            }
            for key in &msg.unset_env {
                command.env_remove(key);
            }
            if let Some(log_path) = &msg.log_path {
                if let Some(parent) = std::path::Path::new(log_path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                {
                    if let Ok(stderr_file) = file.try_clone() {
                        command.stdout(std::process::Stdio::from(file));
                        command.stderr(std::process::Stdio::from(stderr_file));
                    }
                }
            }
            if let Err(err) = command.spawn() {
                spawn_trace(format!("spawn failed program={} err={err}", msg.program));
                response.error = Some(err.to_string());
            } else {
                spawn_trace(format!("spawn ok program={}", msg.program));
                response.ok = true;
            }
            if let Ok(line) = serde_json::to_string(&response) {
                let _ = stream.write_all(line.as_bytes());
                let _ = stream.write_all(b"\n");
                let _ = stream.flush();
            }
        });
    }
}

fn start_daemon_child() {
    spawn_notice(format!("starting spawn daemon at {}", socket_path().display()));
    spawn_trace("starting daemon child");
    match unsafe { libc::fork() } {
        -1 => {}
        0 => {
            if run_daemon().is_err() {
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        _ => {
            for _ in 0..100 {
                if daemon_reachable() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Fork the spawn daemon once per process, before EGL/GLES initialization.
pub fn ensure_daemon() {
    static START: Once = Once::new();
    START.call_once(|| {
        spawn_notice(format!("ensure_daemon socket={}", socket_path().display()));
        spawn_trace("ensure_daemon");
        if daemon_reachable() {
            return;
        }
        start_daemon_child();
    });
}

pub fn request_spawn(msg: &SpawnMessage) -> std::io::Result<()> {
    let socket = socket_path();
    spawn_trace(format!(
        "request_spawn program={} args={} socket={}",
        msg.program,
        msg.args.len(),
        socket.display()
    ));
    let mut stream = UnixStream::connect(&socket).map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!("spawn daemon unreachable at {}: {err}", socket.display()),
        )
    })?;
    stream
        .set_write_timeout(Some(Duration::from_millis(500)))
        .ok();
    let line = serde_json::to_string(msg)? + "\n";
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let Some(line) = response.lines().find(|line| !line.trim().is_empty()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("spawn daemon returned no response for {}", msg.program),
        ));
    };
    let reply = serde_json::from_str::<SpawnResponse>(line).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid spawn response for {}: {err}", msg.program),
        )
    })?;
    if !reply.ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            reply.error.unwrap_or_else(|| "spawn failed".to_string()),
        ));
    }
    spawn_trace(format!("request_spawn sent program={}", msg.program));
    Ok(())
}
