//! PAM transaction handling.
//!
//! libpam is blocking and its conversation callback is synchronous, so each
//! transaction runs on a dedicated std thread. The thread stays alive for
//! the interactive part (authenticate/acct_mgmt); once that's done, opening
//! the actual logind session and launching the target program happens via
//! `fork_session_holder`, not on this thread directly.
//!
//! Why: `pam_systemd`'s session-open hook migrates whichever process calls
//! it into a new `session-cNN.scope` cgroup. If we called `open_session`
//! here, in the daemon, *this whole daemon* would get migrated out of its
//! own service cgroup and into that transient scope -- which is exactly the
//! bug this module works around (see git history for the DRM-master
//! permission failures that caused). Instead, `fork_session_holder` forks a
//! short-lived holder process to own that migration, which then forks again
//! into the real target program (the greeter, or focaldesk) so *that*
//! process tree -- not the daemon -- ends up in the session's scope with a
//! real seat.
//!
//! Thread/process lifecycle:
//!   authenticate -> acct_mgmt (on a dedicated thread, interactive)
//!     -> send Outcome::Success(PendingSession) to the async side
//!     -> park until the supervisor confirms the seat is free
//!     -> fork_session_holder(ctx, exec):
//!          holder opens the session, forks into `exec`, waits for it,
//!          then closes the session -- entirely decoupled from this
//!          daemon's own threads from that point on.

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use anyhow::Context as _;
use nix::fcntl::OFlag;
use nix::unistd::ForkResult;
use pam_client::ConversationHandler;
use pam_client::{Context, Flag};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::ipc::AuthMessageStyle;

/// A message from PAM to show the user. For prompt styles, `reply` carries
/// the channel the answer must be sent on; for info/error it is None.
pub struct PamPrompt {
    pub style: AuthMessageStyle,
    pub message: String,
    pub reply: Option<oneshot::Sender<Zeroizing<String>>>,
}

/// Everything the supervisor needs to track the authenticated user's
/// session process.
pub struct AuthedUser {
    pub username: String,
    pub process: SessionProcess,
}

pub enum Outcome {
    Success(PendingSession),
    Failure { message: String },
}

/// A PAM transaction that has authenticated and passed account checks, but
/// has deliberately not opened the logind session yet -- `open_session`
/// registers the session on the seat/VT, which must not happen until
/// whatever previously occupied that seat/VT (e.g. the greeter) has been
/// fully torn down. Call `open` once that's confirmed.
pub struct PendingSession {
    proceed_tx: oneshot::Sender<Config>,
    result_rx: oneshot::Receiver<anyhow::Result<Box<AuthedUser>>>,
}

impl PendingSession {
    /// Tells the parked PAM thread it's safe to open the session now (the
    /// greeter's own session/seat is confirmed torn down) and waits for the
    /// launched session's process.
    pub async fn open(self, cfg: Config) -> anyhow::Result<Box<AuthedUser>> {
        let _ = self.proceed_tx.send(cfg);
        self.result_rx
            .await
            .context("pam thread died before opening session")?
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

/// Conversation handler for non-interactive PAM services (`pam_permit` in
/// the auth/account stanzas — nothing should ever prompt). Used to open a
/// session for the greeter's own service account, not an authenticating
/// human.
struct NullConversation;

impl ConversationHandler for NullConversation {
    fn prompt_echo_on(&mut self, msg: &CStr) -> Result<CString, pam_client::ErrorCode> {
        tracing::warn!(msg = %msg.to_string_lossy(), "unexpected prompt on non-interactive PAM session");
        Err(pam_client::ErrorCode::CONV_ERR)
    }

    fn prompt_echo_off(&mut self, msg: &CStr) -> Result<CString, pam_client::ErrorCode> {
        tracing::warn!(msg = %msg.to_string_lossy(), "unexpected prompt on non-interactive PAM session");
        Err(pam_client::ErrorCode::CONV_ERR)
    }

    fn text_info(&mut self, msg: &CStr) {
        tracing::debug!(msg = %msg.to_string_lossy(), "pam info");
    }

    fn error_msg(&mut self, msg: &CStr) {
        tracing::warn!(msg = %msg.to_string_lossy(), "pam error");
    }
}

/// Launch directions for the process a session holder execs into once its
/// PAM/logind session is open. Plain data, not a closure -- it must survive
/// a `fork()` into a child that builds real CStrings/argv/envp before
/// `execve`.
pub struct ExecSpec {
    pub program: String,
    /// Applied first; PAM's own session env (XDG_RUNTIME_DIR,
    /// XDG_SESSION_ID, ...) is merged on top once the session is open,
    /// overriding any matching keys here.
    pub env: Vec<(String, String)>,
    pub current_dir: Option<std::path::PathBuf>,
    pub uid: nix::unistd::Uid,
    pub gid: nix::unistd::Gid,
    pub username: String,
}

/// A supervised process whose PAM/logind session is owned by an
/// intermediate "session holder" process (see `fork_session_holder`)
/// rather than by this daemon, so `pam_systemd`'s session-scope cgroup
/// migration never touches focaldmd itself.
pub struct SessionProcess {
    /// PID of the actual exec'd program -- signal this one directly to
    /// terminate it.
    pub pid: i32,
    exit_rx: oneshot::Receiver<ExitStatus>,
    exit_status: Option<ExitStatus>,
    /// Fires once the holder has closed the PAM/logind session and exited.
    closed_rx: oneshot::Receiver<()>,
}

impl SessionProcess {
    /// Waits for the exec'd program to exit. Safe to call repeatedly
    /// (mirroring `tokio::process::Child::wait`): once resolved, the status
    /// is cached and returned immediately on later calls.
    pub async fn wait(&mut self) -> anyhow::Result<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let status = (&mut self.exit_rx)
            .await
            .context("session holder died before reporting an exit status")?;
        self.exit_status = Some(status);
        Ok(status)
    }

    /// Waits for the PAM/logind session to actually finish closing. Call
    /// only after `wait()` has observed the process exit.
    pub async fn closed(self) {
        let _ = self.closed_rx.await;
    }
}

/// Reads exactly `buf.len()` bytes, or returns `Ok(false)` if the writer
/// closed before sending anything -- used to detect the session holder
/// failing before it could report a result.
fn read_exact_or_eof(fd: &OwnedFd, buf: &mut [u8]) -> anyhow::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match nix::unistd::read(fd.as_raw_fd(), &mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e).context("read from session-holder pipe"),
        }
    }
    Ok(true)
}

/// Best-effort write for use in a forked child: there's no async runtime
/// left to report failures to, so errors are simply dropped.
fn write_all_best_effort(fd: &OwnedFd, mut buf: &[u8]) {
    while !buf.is_empty() {
        match nix::unistd::write(fd, buf) {
            Ok(0) => return,
            Ok(n) => buf = &buf[n..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return,
        }
    }
}

/// Diagnostic for the forked-child error paths below, which have no async
/// runtime (and thus no `tracing`) left to report through: writes straight
/// to fd 2, which is still whatever this process inherited (systemd's
/// journal capture, ultimately) since no one has touched it.
fn eprint_child(msg: &str) {
    unsafe {
        libc::write(2, msg.as_ptr().cast(), msg.len());
    }
}

/// Opens a PAM session for `username` under `service`, then forks into
/// `exec` running as that user. `service` is expected to use `pam_permit`
/// for auth/account (root is trusted to vouch for its own greeter account)
/// and `pam_systemd` in its session stack so the account gets a real seat —
/// without this the greeter is just a setuid child with no logind session,
/// and libseat's logind backend refuses it device access.
pub fn open_service_session(
    service: &str,
    username: &str,
    tty: &str,
    exec: ExecSpec,
) -> anyhow::Result<SessionProcess> {
    let mut ctx = Context::new(service, Some(username), NullConversation).context("pam ctx")?;
    ctx.set_tty(Some(tty)).context("set tty")?;
    ctx.authenticate(Flag::NONE).context("authenticate")?;
    ctx.acct_mgmt(Flag::NONE).context("acct_mgmt")?;
    fork_session_holder(ctx, exec)
}

/// Forks a "session holder" process that opens `ctx`'s PAM/logind session,
/// forks *again* into `exec` (as `exec.uid`/`exec.gid`), waits for that
/// process to exit, then closes the session.
///
/// Two forks, not one: `pam_systemd`'s session-open hook migrates whichever
/// process calls it into a new `session-cNN.scope` cgroup. The first fork
/// isolates that migration to a short-lived holder instead of this daemon;
/// the second fork+exec (inheriting the holder's cgroup via ordinary fork
/// semantics) lands the real seat-owning process -- the greeter or
/// focaldesk -- in the scope pam_systemd actually intended for it.
///
/// Must be called from the thread that owns `ctx`, already past
/// `authenticate`/`acct_mgmt`: fork() duplicates the whole process, so the
/// holder gets its own independent copy of the PAM handle to run
/// `open_session`/close on, on its own schedule, entirely decoupled from
/// this daemon's threads from that point on.
fn fork_session_holder<C: ConversationHandler>(
    ctx: Context<C>,
    exec: ExecSpec,
) -> anyhow::Result<SessionProcess> {
    let (pid_r, pid_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).context("pipe")?;
    let (status_r, status_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).context("pipe")?;

    match unsafe { nix::unistd::fork() }.context("fork session holder")? {
        ForkResult::Parent { child } => {
            drop(pid_w);
            drop(status_w);
            // The holder owns the real PAM lifecycle from here; don't let
            // this redundant copy's Drop also run pam_end behind its back.
            std::mem::forget(ctx);

            let mut buf = [0u8; 4];
            if !read_exact_or_eof(&pid_r, &mut buf)? {
                let _ = nix::sys::wait::waitpid(child, None);
                anyhow::bail!("session holder exited before opening the session");
            }
            let pid = i32::from_ne_bytes(buf);

            let (exit_tx, exit_rx) = oneshot::channel();
            let (closed_tx, closed_rx) = oneshot::channel();
            std::thread::Builder::new()
                .name(format!("session-holder-{child}"))
                .spawn(move || {
                    let mut sbuf = [0u8; 4];
                    if !read_exact_or_eof(&status_r, &mut sbuf).unwrap_or(false) {
                        // Holder died without reporting -- nothing sane to
                        // send; the supervisor sees the channel close.
                        return;
                    }
                    let status = ExitStatus::from_raw(i32::from_ne_bytes(sbuf));
                    let _ = exit_tx.send(status);
                    let _ = nix::sys::wait::waitpid(child, None);
                    let _ = closed_tx.send(());
                })
                .context("spawn session-holder relay thread")?;

            Ok(SessionProcess {
                pid,
                exit_rx,
                exit_status: None,
                closed_rx,
            })
        }
        ForkResult::Child => {
            drop(pid_r);
            drop(status_r);
            run_session_holder_child(ctx, exec, pid_w, status_w);
        }
    }
}

/// Runs as the session holder: opens `ctx`'s session, forks into `exec`,
/// waits for it, reports back over the pipes, then closes the session.
/// Never returns -- every path ends in `_exit`.
fn run_session_holder_child<C: ConversationHandler>(
    mut ctx: Context<C>,
    exec: ExecSpec,
    pid_w: OwnedFd,
    status_w: OwnedFd,
) -> ! {
    let session = match ctx.open_session(Flag::NONE) {
        Ok(s) => s,
        // Closing pid_w without writing tells the parent this failed; the
        // parent only sees a generic "exited before opening", so put the
        // real PAM error on stderr too.
        Err(e) => {
            eprint_child(&format!("focaldmd: open_session failed: {e}\n"));
            unsafe { libc::_exit(1) };
        }
    };

    // Fold PAM's session env on top of our defaults -- pam_systemd's
    // XDG_RUNTIME_DIR/XDG_SESSION_ID/etc. must win.
    let mut env = exec.env;
    for (k, v) in session.envlist().iter_tuples() {
        let k = k.to_string_lossy().into_owned();
        let v = v.to_string_lossy().into_owned();
        match env.iter_mut().find(|(ek, _)| *ek == k) {
            Some(slot) => slot.1 = v,
            None => env.push((k, v)),
        }
    }

    match unsafe { nix::unistd::fork() } {
        Ok(ForkResult::Child) => {
            // The exec'd program inherits none of this: no PAM handle, and
            // the pipes are O_CLOEXEC so they close across the exec below.
            drop(session);
            drop(ctx);
            exec_into(
                &exec.program,
                &env,
                exec.current_dir.as_deref(),
                exec.uid,
                exec.gid,
                &exec.username,
            );
        }
        Ok(ForkResult::Parent { child }) => {
            write_all_best_effort(&pid_w, &child.as_raw().to_ne_bytes());
            drop(pid_w);

            // Raw libc, not nix's decoded WaitStatus: we want the exact
            // wait(2) status word so the parent can reconstruct a real
            // `std::process::ExitStatus` with `from_raw`.
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(child.as_raw(), &mut status, 0) };
            write_all_best_effort(&status_w, &status.to_ne_bytes());
            drop(status_w);

            drop(session); // pam_close_session
            drop(ctx); // pam_end
            unsafe { libc::_exit(0) };
        }
        Err(_) => unsafe { libc::_exit(1) },
    }
}

/// Drops privileges to `uid`/`gid` and execs `program` with `env` as the
/// complete environment (no inheritance from this process). Never returns.
fn exec_into(
    program: &str,
    env: &[(String, String)],
    current_dir: Option<&std::path::Path>,
    uid: nix::unistd::Uid,
    gid: nix::unistd::Gid,
    username: &str,
) -> ! {
    // stdin null, matching the original Stdio::null() setup.
    if let Ok(null) = std::fs::File::open("/dev/null") {
        unsafe { libc::dup2(null.as_raw_fd(), 0) };
    }

    if let Some(dir) = current_dir {
        let _ = nix::unistd::chdir(dir);
    }

    let Ok(name) = CString::new(username) else {
        eprint_child("focaldmd: username contains NUL\n");
        unsafe { libc::_exit(1) };
    };
    // Order matters: setuid last, or we lose the privilege to do the rest.
    if let Err(e) = nix::unistd::setgid(gid) {
        eprint_child(&format!("focaldmd: setgid failed: {e}\n"));
        unsafe { libc::_exit(1) };
    }
    if let Err(e) = nix::unistd::initgroups(&name, gid) {
        eprint_child(&format!("focaldmd: initgroups failed: {e}\n"));
        unsafe { libc::_exit(1) };
    }
    if let Err(e) = nix::unistd::setuid(uid) {
        eprint_child(&format!("focaldmd: setuid failed: {e}\n"));
        unsafe { libc::_exit(1) };
    }

    let Ok(path) = CString::new(program) else {
        eprint_child("focaldmd: program path contains NUL\n");
        unsafe { libc::_exit(1) };
    };
    let argv = [path.clone()];
    let envp: Vec<CString> = env
        .iter()
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();

    let err = nix::unistd::execve(&path, &argv, &envp).unwrap_err();
    eprint_child(&format!("focaldmd: execve {program} failed: {err}\n"));
    unsafe { libc::_exit(127) };
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

/// Builds the launch directions for the authenticated user's desktop
/// session. PAM's own session env (XDG_RUNTIME_DIR, XDG_SESSION_ID, ...) is
/// merged on top by the session holder once it opens the session.
fn session_exec_spec(cfg: &Config, user: &nix::unistd::User) -> ExecSpec {
    let mut env = vec![
        ("HOME".to_string(), user.dir.to_string_lossy().into_owned()),
        ("USER".to_string(), user.name.clone()),
        ("LOGNAME".to_string(), user.name.clone()),
        (
            "SHELL".to_string(),
            user.shell.to_string_lossy().into_owned(),
        ),
        ("XDG_SESSION_TYPE".to_string(), "wayland".to_string()),
        ("XDG_CURRENT_DESKTOP".to_string(), "focaldesk".to_string()),
        ("XDG_SEAT".to_string(), "seat0".to_string()),
        ("XDG_VTNR".to_string(), cfg.vt.to_string()),
        (
            "XKB_DEFAULT_LAYOUT".to_string(),
            cfg.keyboard_layout.clone(),
        ),
    ];
    // Empty means "no override" -- xkbcommon falls back to its own default
    // when these are unset, which an empty env var would not reliably do.
    if !cfg.keyboard_variant.is_empty() {
        env.push((
            "XKB_DEFAULT_VARIANT".to_string(),
            cfg.keyboard_variant.clone(),
        ));
    }
    if !cfg.keyboard_model.is_empty() {
        env.push(("XKB_DEFAULT_MODEL".to_string(), cfg.keyboard_model.clone()));
    }
    if !cfg.keyboard_options.is_empty() {
        env.push((
            "XKB_DEFAULT_OPTIONS".to_string(),
            cfg.keyboard_options.clone(),
        ));
    }
    // focaldmd starts the desktop with a complete, clean environment rather
    // than inheriting its own service environment. Apply administrator
    // session overrides last so compositor feature flags can be configured
    // in focaldmd.toml. PAM's session environment is still merged afterward.
    for (key, value) in &cfg.session_environment {
        match env.iter_mut().find(|(existing, _)| existing == key) {
            Some(slot) => slot.1 = value.clone(),
            None => env.push((key.clone(), value.clone())),
        }
    }

    ExecSpec {
        program: cfg.session_cmd.clone(),
        env,
        current_dir: Some(user.dir.clone()),
        uid: user.uid,
        gid: user.gid,
        username: user.name.clone(),
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
        ctx.authenticate(Flag::NONE)
            .context("authentication failed")?;
        // Expired passwords, access restrictions, etc.
        ctx.acct_mgmt(Flag::NONE).context("account check failed")?;
        Ok(ctx)
    })();

    let ctx = match attempt {
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

    // Credentials are verified, but we deliberately don't open the session
    // yet: it registers on the seat/VT, and the greeter's own session may
    // still be occupying it. Hand a PendingSession back to the supervisor
    // and wait for it to confirm the seat is free (and hand back the
    // config needed to build the session's launch directions).
    let (proceed_tx, proceed_rx) = oneshot::channel::<Config>();
    let (result_tx, result_rx) = oneshot::channel::<anyhow::Result<Box<AuthedUser>>>();

    if outcome_tx
        .send(Outcome::Success(PendingSession {
            proceed_tx,
            result_rx,
        }))
        .is_err()
    {
        // Supervisor gave up (e.g. greeter crashed) before even asking us
        // to proceed — nothing was opened, so there's nothing to close.
        return Ok(());
    }

    let cfg = match proceed_rx.blocking_recv() {
        Ok(cfg) => cfg,
        // Supervisor dropped the PendingSession without opening — done.
        Err(_) => return Ok(()),
    };

    let exec = session_exec_spec(&cfg, &user);
    let result = fork_session_holder(ctx, exec).map(|process| {
        Box::new(AuthedUser {
            username: user.name.clone(),
            process,
        })
    });
    // Supervisor may have given up in the meantime; nothing more to do
    // either way -- the holder (if one was started) runs independently.
    let _ = result_tx.send(result);
    Ok(())
}
