//! The supervision loop: greeter -> auth -> handoff -> session -> greeter.

use std::ffi::CString;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::process::{Child, Command};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::ipc::{Listener, Request, Response};
use crate::pam::{self, AuthedUser, Outcome, PamPrompt, PamTask};

/// Exponential backoff for greeter respawn so a crashing greeter can't
/// spin tty1 at 100% CPU.
struct Backoff {
    last_spawn: Instant,
    delay: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            last_spawn: Instant::now() - Duration::from_secs(60),
            delay: Duration::from_millis(200),
        }
    }

    fn next_delay(&mut self) -> Duration {
        // Survived >10s? Healthy — reset.
        if self.last_spawn.elapsed() > Duration::from_secs(10) {
            self.delay = Duration::from_millis(200);
        } else {
            self.delay = (self.delay * 2).min(Duration::from_secs(10));
        }
        self.last_spawn = Instant::now();
        self.delay
    }
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let greeter_user = nix::unistd::User::from_name(&cfg.greeter_user)
        .context("getpwnam greeter")?
        .with_context(|| format!("greeter user '{}' does not exist", cfg.greeter_user))?;

    let listener = Listener::bind(&cfg.socket_path, greeter_user.uid)?;
    let mut backoff = Backoff::new();

    loop {
        // ---- Greeter phase -------------------------------------------------
        tokio::time::sleep(backoff.next_delay()).await;
        let greeter = spawn_greeter(&cfg, &greeter_user)?;
        tracing::info!(pid = greeter.id(), "greeter started");

        let authed = match drive_greeter(&cfg, &listener, greeter).await? {
            GreeterPhase::Crashed => continue,
            GreeterPhase::Authenticated { greeter, authed } => {
                // ---- Critical handoff --------------------------------------
                // Greeter and session share the VT. The greeter must be
                // fully reaped (DRM master released) before focaldesk runs.
                terminate_and_reap(greeter).await?;
                authed
            }
        };

        // ---- Session phase -------------------------------------------------
        tracing::info!(user = %authed.username, "launching session");
        match spawn_user_session(&cfg, &authed) {
            Ok(mut session) => {
                let status = session.wait().await?;
                tracing::info!(?status, "session exited");
            }
            Err(e) => tracing::error!(error = %e, "failed to launch session"),
        }

        // AuthedUser drops here: SessionGuard signals the parked PAM
        // thread, pam_close_session runs, logind tears the session down.
        drop(authed);
    }
}

enum GreeterPhase {
    Crashed,
    Authenticated {
        greeter: Child,
        authed: Box<AuthedUser>,
    },
}

/// Runs while the greeter is on screen. Multiplexes:
///   - greeter process exit (crash -> respawn)
///   - IPC requests from the greeter
///   - prompts from an in-flight PAM transaction
async fn drive_greeter(
    cfg: &Config,
    listener: &Listener,
    mut greeter: Child,
) -> anyhow::Result<GreeterPhase> {
    // Accept the greeter's connection, but keep watching the process:
    // if it dies before connecting we'd otherwise hang in accept.
    let mut conn = tokio::select! {
        conn = listener.accept_greeter() => conn?,
        status = greeter.wait() => {
            tracing::warn!(?status, "greeter exited before connecting");
            return Ok(GreeterPhase::Crashed);
        }
    };

    // In-flight PAM transaction, if any (receivers held separately so the
    // select! arms below each borrow only what they need).
    let mut prompt_rx: Option<tokio::sync::mpsc::Receiver<PamPrompt>> = None;
    let mut outcome_rx: Option<tokio::sync::oneshot::Receiver<Outcome>> = None;
    // The reply channel for the prompt currently shown in the greeter.
    let mut pending_reply: Option<tokio::sync::oneshot::Sender<Zeroizing<String>>> = None;

    loop {
        tokio::select! {
            // Greeter process died (crash, or we're mid-shutdown elsewhere).
            status = greeter.wait() => {
                tracing::warn!(?status, "greeter exited unexpectedly");
                return Ok(GreeterPhase::Crashed);
            }

            // Message from the greeter.
            req = conn.recv() => match req? {
                Request::CreateSession { username } => {
                    if prompt_rx.is_some() {
                        conn.send(&Response::AuthError {
                            message: "authentication already in progress".into(),
                        }).await?;
                        continue;
                    }
                    let PamTask { prompt_rx: p, outcome_rx: o } = pam::start(
                        cfg.pam_service.clone(),
                        username,
                        cfg.tty_name.clone(),
                    );
                    prompt_rx = Some(p);
                    outcome_rx = Some(o);
                }
                Request::PostAuthResponse { response } => {
                    match (pending_reply.take(), response) {
                        (Some(reply), Some(text)) => {
                            // Failure means PAM thread died; outcome branch
                            // below will surface the error.
                            let _ = reply.send(Zeroizing::new(text));
                        }
                        (Some(reply), None) => drop(reply), // user cancelled prompt
                        (None, _) => tracing::warn!("unsolicited auth response"),
                    }
                }
                Request::CancelSession => {
                    // Dropping the receivers closes the prompt channel;
                    // PAM's conversation errors out and the thread unwinds.
                    prompt_rx = None;
                    outcome_rx = None;
                    pending_reply = None;
                }
            },

            // PAM wants to show something / ask something.
            Some(PamPrompt { style, message, reply }) = recv_prompt(&mut prompt_rx) => {
                pending_reply = reply;
                conn.send(&Response::AuthMessage { style, message }).await?;
            }

            // PAM transaction finished.
            Some(outcome) = recv_outcome(&mut outcome_rx) => match outcome {
                Outcome::Success(authed) => {
                    conn.send(&Response::SessionStarted).await?;
                    return Ok(GreeterPhase::Authenticated { greeter, authed });
                }
                Outcome::Failure { message } => {
                    tracing::info!(%message, "authentication failed");
                    conn.send(&Response::AuthError { message }).await?;
                    prompt_rx = None;
                    outcome_rx = None;
                    pending_reply = None;
                }
            },
        }
    }
}

/// select!-friendly helpers: resolve only while a transaction is in flight.
async fn recv_prompt(rx: &mut Option<tokio::sync::mpsc::Receiver<PamPrompt>>) -> Option<PamPrompt> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_outcome(rx: &mut Option<tokio::sync::oneshot::Receiver<Outcome>>) -> Option<Outcome> {
    match rx {
        Some(rx) => rx.await.ok(),
        None => std::future::pending().await,
    }
}

/// SIGTERM -> bounded wait -> SIGKILL. Returning from this function is the
/// invariant that the VT and DRM master are free for the next compositor.
async fn terminate_and_reap(mut child: Child) -> anyhow::Result<()> {
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
        Ok(status) => tracing::debug!(?status, "greeter reaped"),
        Err(_) => {
            tracing::warn!("greeter ignored SIGTERM; killing");
            child.kill().await?; // SIGKILL + reap
        }
    }
    Ok(())
}

fn spawn_greeter(cfg: &Config, user: &nix::unistd::User) -> anyhow::Result<Child> {
    let mut cmd = Command::new(&cfg.greeter_cmd);
    cmd.env_clear()
        .env("FOCALDM_SOCKET", &cfg.socket_path)
        // Greeter user gets a static runtime dir created by systemd-tmpfiles
        // or the unit (RuntimeDirectory=); it has no logind session.
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", user.uid))
        .stdin(Stdio::null())
        .kill_on_drop(true);

    drop_privileges(&mut cmd, user.uid, user.gid, &user.name);
    Ok(cmd.spawn().context("spawn greeter")?)
}

fn spawn_user_session(cfg: &Config, s: &AuthedUser) -> anyhow::Result<Child> {
    let mut cmd = Command::new(&cfg.session_cmd); // from config — NEVER from IPC
    cmd.env_clear()
        .env("HOME", &s.home)
        .env("USER", &s.username)
        .env("LOGNAME", &s.username)
        .env("SHELL", &s.shell)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "focaldesk")
        .env("XDG_SEAT", "seat0")
        .env("XDG_VTNR", cfg.vt.to_string())
        .env("XKB_DEFAULT_LAYOUT", &cfg.keyboard_layout)
        .current_dir(&s.home)
        .stdin(Stdio::null());

    // Empty means "no override" — xkbcommon falls back to its own default
    // when these are unset, which an empty env var would not reliably do.
    if !cfg.keyboard_variant.is_empty() {
        cmd.env("XKB_DEFAULT_VARIANT", &cfg.keyboard_variant);
    }
    if !cfg.keyboard_model.is_empty() {
        cmd.env("XKB_DEFAULT_MODEL", &cfg.keyboard_model);
    }
    if !cfg.keyboard_options.is_empty() {
        cmd.env("XKB_DEFAULT_OPTIONS", &cfg.keyboard_options);
    }

    // PAM env wins over our defaults: XDG_RUNTIME_DIR / XDG_SESSION_ID
    // come from pam_systemd, which is the authoritative source.
    for (k, v) in &s.pam_env {
        cmd.env(k, v);
    }

    drop_privileges(&mut cmd, s.uid, s.gid, &s.username);
    Ok(cmd.spawn().context("spawn session")?)
}

/// pre_exec runs in the forked child before exec. Only async-signal-safe
/// calls allowed: setgid/initgroups/setuid qualify. ORDER MATTERS —
/// setuid last, or we lose the privilege to do the rest.
fn drop_privileges(cmd: &mut Command, uid: nix::unistd::Uid, gid: nix::unistd::Gid, name: &str) {
    let name = CString::new(name).expect("username with NUL");
    unsafe {
        cmd.pre_exec(move || {
            nix::unistd::setgid(gid)?;
            nix::unistd::initgroups(&name, gid)?;
            nix::unistd::setuid(uid)?;
            Ok(())
        });
    }
}
