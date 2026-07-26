// focal-launchd/src/server.rs

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use focal_launch_shared::{
    BrowserBackend, LaunchRequest, LaunchResponse, chrome_command_args, is_browser_like,
    is_chrome_like, socket_path,
};
use focaldesk_ipc::transport;
use focaldesk_logging::log_file_path_candidates;

pub fn run() -> anyhow::Result<()> {
    let socket = socket_path()?;
    let listener = transport::bind_user_socket(&socket)?;

    for stream in listener.incoming() {
        let mut stream = stream?;
        thread::spawn(move || {
            if let Err(err) = transport::require_authorized_peer(&stream, transport::LAUNCH_POLICY)
            {
                let response = LaunchResponse::Failed {
                    message: err.to_string(),
                };
                if let Ok(json) = transport::encode_message(&response) {
                    let _ = stream.write_all(&json);
                }
                return;
            }
            let response = match handle_stream(&mut stream) {
                Ok(response) => response,
                Err(err) => LaunchResponse::Failed {
                    message: err.to_string(),
                },
            };

            let json = transport::encode_message(&response).unwrap();
            let _ = stream.write_all(&json);
            let _ = stream.write_all(b"\n");
        });
    }

    Ok(())
}

fn handle_stream(stream: &mut std::os::unix::net::UnixStream) -> anyhow::Result<LaunchResponse> {
    let payload = transport::read_limited(stream)?;
    let req: LaunchRequest = transport::decode_message(&payload).map_err(anyhow::Error::msg)?;
    eprintln!(
        "focal-launchd: accepted launch trace_id={} app={}",
        req.trace_id, req.app
    );
    thread::spawn(move || {
        if let Err(err) = launch(req) {
            eprintln!("focal-launchd: launch failed: {err:#}");
        }
    });

    Ok(LaunchResponse::Accepted)
}

fn launch(req: LaunchRequest) -> anyhow::Result<()> {
    let browser_like = is_browser_like(&req.app);
    let chrome_like = is_chrome_like(&req.app);

    let prefer_x11 = matches!(req.browser_backend, BrowserBackend::Xwayland);

    let mut cmd = Command::new(&req.app);

    if browser_like {
        if prefer_x11 {
            if let Some(display) = &req.xwayland_display {
                cmd.env_remove("WAYLAND_DISPLAY");
                cmd.env("DISPLAY", display);
            }
        } else {
            cmd.env("WAYLAND_DISPLAY", &req.wayland_display);
            cmd.env_remove("DISPLAY");
        }
    } else {
        cmd.env("WAYLAND_DISPLAY", &req.wayland_display);
        if let Some(display) = &req.xwayland_display {
            cmd.env("DISPLAY", display);
        }
    }

    if chrome_like {
        let profile = chrome_profile_dir();
        clear_stale_chrome_singleton(&profile);
        let profile = profile.to_string_lossy();
        cmd.args(chrome_command_args(prefer_x11, &profile));
    }

    cmd.args(req.args);

    if chrome_like || browser_like {
        if let Some(log_path) = launch_trace_path() {
            if let Some(parent) = Path::new(&log_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                if let Ok(stderr_file) = file.try_clone() {
                    cmd.stdout(std::process::Stdio::from(file));
                    cmd.stderr(std::process::Stdio::from(stderr_file));
                }
            }
        }
    }

    eprintln!(
        "focal-launchd: spawning trace_id={} app={} browser_like={} chrome_like={}",
        req.trace_id, req.app, browser_like, chrome_like
    );
    match cmd.spawn() {
        Ok(child) => {
            eprintln!(
                "focal-launchd: spawn ok trace_id={} app={} pid={}",
                req.trace_id,
                req.app,
                child.id()
            );
        }
        Err(err) => {
            eprintln!(
                "focal-launchd: spawn failed trace_id={} app={} err={}",
                req.trace_id, req.app, err
            );
            return Err(err.into());
        }
    }

    Ok(())
}

fn chrome_profile_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        })
        .join("focaldesk")
        .join("chrome-profile")
}

fn launch_trace_path() -> Option<String> {
    log_file_path_candidates()
        .into_iter()
        .next()
        .map(|path| path.to_string_lossy().into_owned())
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
