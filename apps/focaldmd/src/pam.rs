//! PAM transaction handling.
//!
//! libpam is blocking and its conversation callback is synchronous, so each
//! transaction runs on a dedicated std thread. The thread stays alive for
//! the entire user session: the PAM context should not migrate threads, and
//! pam_close_session must run on it when focaldesk exits.
//!
//! Thread lifecycle:
//!   authenticate -> acct_mgmt -> open_session
//!     -> send Outcome::Success { env, uid, gid, guard } to the async side
//!     -> park on the close channel
//!     -> on signal (guard dropped): pam_close_session runs, thread exits

use std::ffi::{CStr, CString};

use anyhow::Context as _;
use pam_client::ConversationHandler;
use pam_client::{Context, Flag};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::ipc::AuthMessageStyle;

/// A message from PAM to show the user. For prompt styles, `reply` carries
/// the channel the answer must be sent on; for info/error it is None.
pub struct PamPrompt {
    pub style: AuthMessageStyle,
    pub message: String,
    pub reply: Option<oneshot::Sender<Zeroizing<String>>>,
}

/// Everything the supervisor needs to launch the session.
pub struct AuthedUser {
    pub username: String,
    pub uid: nix::unistd::Uid,
    pub gid: nix::unistd::Gid,
    pub home: std::path::PathBuf,
    pub shell: std::path::PathBuf,
    /// Environment exported by PAM modules — pam_systemd sets
    /// XDG_RUNTIME_DIR, XDG_SESSION_ID, etc. Apply verbatim to the child.
    pub pam_env: Vec<(String, String)>,
    /// While this lives, the PAM/logind session is open. Dropping it runs
    /// pam_close_session on the PAM thread.
    pub session_guard: SessionGuard,
}

pub enum Outcome {
    Success(Box<AuthedUser>),
    Failure { message: String },
}

/// RAII: signals the parked PAM thread to close the session.
pub struct SessionGuard {
    close_tx: Option<oneshot::Sender<()>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Bridges PAM's synchronous conversation to the async IPC loop. Each
/// prompt blocks this (dedicated) thread until the greeter answers.
struct GreeterConversation {
    prompt_tx: mpsc::Sender<PamPrompt>,
}

impl GreeterConversation {
    fn ask(
        &mut self,
        style: AuthMessageStyle,
        msg: &CStr,
    ) -> Result<Zeroizing<String>, pam_client::ErrorCode> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.prompt_tx
            .blocking_send(PamPrompt {
                style,
                message: msg.to_string_lossy().into_owned(),
                reply: Some(reply_tx),
            })
            .map_err(|_| pam_client::ErrorCode::CONV_ERR)?;
        // Blocks while the human types. This is why we own a whole thread.
        reply_rx
            .blocking_recv()
            .map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    fn tell(&mut self, style: AuthMessageStyle, msg: &CStr) {
        let _ = self.prompt_tx.blocking_send(PamPrompt {
            style,
            message: msg.to_string_lossy().into_owned(),
            reply: None,
        });
    }
}

impl ConversationHandler for GreeterConversation {
    fn prompt_echo_on(&mut self, msg: &CStr) -> Result<CString, pam_client::ErrorCode> {
        let ans = self.ask(AuthMessageStyle::Visible, msg)?;
        CString::new(ans.as_str()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    fn prompt_echo_off(&mut self, msg: &CStr) -> Result<CString, pam_client::ErrorCode> {
        let ans = self.ask(AuthMessageStyle::Secret, msg)?;
        // This CString is handed to pam_authenticate. Our Zeroizing source
        // is wiped when `ans` drops at the end of this frame.
        CString::new(ans.as_str()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    fn text_info(&mut self, msg: &CStr) {
        self.tell(AuthMessageStyle::Info, msg);
    }

    fn error_msg(&mut self, msg: &CStr) {
        self.tell(AuthMessageStyle::Error, msg);
    }
}

pub struct PamTask {
    /// Prompts/messages PAM wants shown to the user.
    pub prompt_rx: mpsc::Receiver<PamPrompt>,
    /// Final result of the transaction.
    pub outcome_rx: oneshot::Receiver<Outcome>,
}

/// Start a PAM transaction for `username` on its own thread.
/// Dropping the returned PamTask cancels: the conversation's next
/// blocking_send fails, PAM aborts with CONV_ERR, thread unwinds cleanly.
pub fn start(service: String, username: String, tty: String) -> PamTask {
    let (prompt_tx, prompt_rx) = mpsc::channel(4);
    let (outcome_tx, outcome_rx) = oneshot::channel();

    std::thread::Builder::new()
        .name(format!("pam-{username}"))
        .spawn(move || {
            if let Err(e) = run_transaction(&service, &username, &tty, prompt_tx, outcome_tx) {
                tracing::debug!(error = %e, "pam transaction ended with error");
            }
        })
        .expect("spawn pam thread");

    PamTask {
        prompt_rx,
        outcome_rx,
    }
}

fn run_transaction(
    service: &str,
    username: &str,
    tty: &str,
    prompt_tx: mpsc::Sender<PamPrompt>,
    outcome_tx: oneshot::Sender<Outcome>,
) -> anyhow::Result<()> {
    let conv = GreeterConversation { prompt_tx };

    let attempt = (|| -> anyhow::Result<_> {
        let mut ctx = Context::new(service, Some(username), conv).context("pam ctx")?;
        ctx.set_tty(Some(tty)).context("set tty")?;

        // Drives the conversation: prompts flow to the greeter and back.
        ctx.authenticate(Flag::NONE).context("authentication failed")?;
        // Expired passwords, access restrictions, etc.
        ctx.acct_mgmt(Flag::NONE).context("account check failed")?;
        Ok(ctx)
    })();

    let mut ctx = match attempt {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = outcome_tx.send(Outcome::Failure {
                message: format!("{e:#}"),
            });
            return Ok(());
        }
    };

    // Canonical user lookup (post-auth — PAM may have remapped the name).
    let user = nix::unistd::User::from_name(username)
        .context("getpwnam")?
        .ok_or_else(|| anyhow::anyhow!("user {username} vanished after auth"))?;

    // pam_systemd registers the logind session here: device ACLs on the
    // seat, XDG_RUNTIME_DIR creation. This is what lets focaldesk take
    // DRM master without focaldmd handing out fds.
    let session = match ctx.open_session(Flag::NONE) {
        Ok(s) => s,
        Err(e) => {
            let _ = outcome_tx.send(Outcome::Failure {
                message: format!("open_session: {e}"),
            });
            return Ok(());
        }
    };

    let pam_env: Vec<(String, String)> = session
        .envlist()
        .iter_tuples()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();

    let (close_tx, close_rx) = oneshot::channel::<()>();

    let authed = AuthedUser {
        username: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
        home: user.dir.clone(),
        shell: user.shell.clone(),
        pam_env,
        session_guard: SessionGuard {
            close_tx: Some(close_tx),
        },
    };

    if outcome_tx.send(Outcome::Success(Box::new(authed))).is_err() {
        // Supervisor gave up (e.g. greeter crashed at the wrong moment).
        // `session` drops here -> pam_close_session immediately.
        return Ok(());
    }

    // Park for the lifetime of the desktop session. Err means the guard
    // was dropped without an explicit send — same meaning: close now.
    let _ = close_rx.blocking_recv();

    drop(session); // pam_close_session + logind teardown
    Ok(())
}
