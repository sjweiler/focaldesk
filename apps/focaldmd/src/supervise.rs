//! The supervision loop: greeter -> auth -> handoff -> session -> greeter.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use zeroize::Zeroizing;

use crate::config::Config;
use crate::ipc::{Listener, Request, Response};
use crate::pam::{self, ExecSpec, Outcome, PamPrompt, PamTask, PendingSession, SessionProcess};

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

        // greetd-equivalent of `[terminal] switch = true`: make the greeter
        // VT the foreground console *before* opening its logind session so
        // libseat TakeDevice returns a DRM-master FD. Without this, the
        // greeter opens card nodes unprivileged and NVIDIA scanout fails.
        if let Err(e) = crate::vt::switch_to(cfg.vt) {
            tracing::error!(error = %e, vt = cfg.vt, "failed to switch to greeter VT");
            continue;
        }

        let exec = ExecSpec {
            program: cfg.greeter_cmd.clone(),
            env: vec![
                (
                    "FOCALDM_SOCKET".to_string(),
                    cfg.socket_path.to_string_lossy().into_owned(),
                ),
                // Fallback only — overridden by the greeter's own PAM
                // session env whenever pam_systemd set XDG_RUNTIME_DIR
                // itself.
                (
                    "XDG_RUNTIME_DIR".to_string(),
                    format!("/run/user/{}", greeter_user.uid),
                ),
                ("FOCALDM_VT".to_string(), cfg.vt.to_string()),
                ("XDG_VTNR".to_string(), cfg.vt.to_string()),
            ],
            current_dir: None,
            uid: greeter_user.uid,
            gid: greeter_user.gid,
            username: greeter_user.name.clone(),
        };

        // The greeter is just a setuid child with no seat of its own unless
        // it has a real logind session: this is what lets it (via libseat)
        // get DRM/input device access on the greeter's VT. Opened fresh for
        // each greeter spawn and torn down with it. The session is opened
        // and the greeter exec'd from a forked holder, not from this
        // daemon, so pam_systemd's cgroup migration lands on the greeter's
        // own process tree instead of on focaldmd itself.
        let greeter = pam::open_service_session(
            &cfg.greeter_pam_service,
            &cfg.greeter_user,
            &cfg.tty_name,
            exec,
        )
        .context("open greeter PAM session")?;
        tracing::info!(pid = greeter.pid, vt = cfg.vt, "greeter started");

        let pending = match drive_greeter(&cfg, &listener, greeter).await? {
            GreeterPhase::Crashed { greeter } => {
                // Wait for the holder to close the session before
                // retrying, so a fresh greeter never overlaps the crashed
                // one's session on the same VT.
                greeter.closed().await;
                continue;
            }
            GreeterPhase::Authenticated { greeter, pending } => {
                // ---- Critical handoff --------------------------------------
                // Greeter and session share the VT. The greeter must be
                // fully reaped (DRM master released) and its own logind
                // session fully closed *before* the user's session opens on
                // the same VT — otherwise the two can briefly coexist.
                terminate_and_reap(greeter).await?;
                pending
            }
        };

        // Only now — with the greeter's seat confirmed released — does the
        // PAM thread actually call open_session for the authenticating user.
        let mut authed = match pending.open(cfg.clone()).await {
            Ok(authed) => authed,
            Err(e) => {
                tracing::error!(error = %e, "failed to open user session");
                continue;
            }
        };

        // ---- Session phase -------------------------------------------------
        tracing::info!(user = %authed.username, pid = authed.process.pid, "launching session");
        match authed.process.wait().await {
            Ok(status) => tracing::info!(?status, "session exited"),
            Err(e) => tracing::error!(error = %e, "session process error"),
        }

        // Waits for pam_close_session to finish before the loop repeats and
        // a fresh greeter session gets opened on the same VT.
        authed.process.closed().await;
    }
}

enum GreeterPhase {
    Crashed {
        greeter: SessionProcess,
    },
    Authenticated {
        greeter: SessionProcess,
        pending: PendingSession,
    },
}

/// Runs while the greeter is on screen. Multiplexes:
///   - greeter process exit (crash -> respawn)
///   - IPC requests from the greeter
///   - prompts from an in-flight PAM transaction
async fn drive_greeter(
    cfg: &Config,
    listener: &Listener,
    mut greeter: SessionProcess,
) -> anyhow::Result<GreeterPhase> {
    // Accept the greeter's connection, but keep watching the process:
    // if it dies before connecting we'd otherwise hang in accept.
    let mut conn = tokio::select! {
        conn = listener.accept_greeter() => conn?,
        status = greeter.wait() => {
            tracing::warn!(?status, "greeter exited before connecting");
            return Ok(GreeterPhase::Crashed { greeter });
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
                return Ok(GreeterPhase::Crashed { greeter });
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
                Outcome::Success(pending) => {
                    conn.send(&Response::SessionStarted).await?;
                    return Ok(GreeterPhase::Authenticated { greeter, pending });
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
/// invariant that the VT and DRM master are free for the next compositor,
/// *and* that the greeter's PAM/logind session has actually closed.
async fn terminate_and_reap(mut greeter: SessionProcess) -> anyhow::Result<()> {
    let pid = nix::unistd::Pid::from_raw(greeter.pid);
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    match tokio::time::timeout(Duration::from_secs(3), greeter.wait()).await {
        Ok(status) => tracing::debug!(?status, "greeter reaped"),
        Err(_) => {
            tracing::warn!("greeter ignored SIGTERM; killing");
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = greeter.wait().await;
        }
    }
    greeter.closed().await;
    Ok(())
}
