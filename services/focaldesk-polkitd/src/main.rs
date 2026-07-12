use anyhow::{Context, Result};
use focaldesk_ipc::dialog::{DialogIpcRequest, DialogIpcResponse};
use focaldesk_logging::{flog_error, flog_info, flog_warn};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, DBusError, dbus_interface, dbus_proxy};
use zbus_polkit::policykit1::{AuthorityProxy, Subject};

const AGENT_OBJECT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";

type LoginSessionEntry = (String, u32, String, String, OwnedObjectPath);

#[dbus_proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    fn list_sessions(&self) -> zbus::Result<Vec<LoginSessionEntry>>;
}

/// Maps to `org.freedesktop.PolicyKit1.Error.*`; polkitd expects `Cancelled`
/// specifically when the user dismisses/declines authentication.
#[derive(Debug, DBusError)]
#[dbus_error(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    #[dbus_error(zbus_error)]
    ZBus(zbus::Error),
    Cancelled(String),
    Failed(String),
}

struct AuthenticationAgent;

#[dbus_interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl AuthenticationAgent {
    /// Per `org.freedesktop.PolicyKit1.AuthenticationAgent`: polkitd calls this and blocks until
    /// we return; on success we must have already called `AuthenticationAgentResponse2` on the
    /// Authority before returning, on decline/cancel we return the `Cancelled` error.
    #[allow(clippy::too_many_arguments)]
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), AgentError> {
        let _ = details;
        flog_info!(
            "polkit BeginAuthentication: action_id={action_id} message={message} cookie={cookie} identities={}",
            identities.len()
        );

        // Picking the first unix-user identity covers the common case (a single
        // identity offered: the requesting user). Actions that legitimately offer
        // a choice of identities (e.g. any wheel-group member, or explicitly root)
        // would need a chooser UI; not needed for the common password-prompt path.
        let Some((_uid, identity_str)) = identities
            .iter()
            .find(|(kind, _)| kind == "unix-user")
            .and_then(|(_, details)| unix_user_uid(details))
        else {
            return Err(AgentError::Failed(
                "no unix-user identity offered for authentication".into(),
            ));
        };

        let gained_authorization =
            run_agent_session(identity_str, cookie.clone(), message, icon_name)
                .await
                .unwrap_or_else(|err| {
                    flog_error!("polkit agent session failed: {err}");
                    false
                });

        if !gained_authorization {
            return Err(AgentError::Cancelled(
                "authentication was cancelled or failed".into(),
            ));
        }

        // PolkitAgentSession's helper reports the successful authentication to
        // polkitd as part of the conversation. Sending AuthenticationAgentResponse2
        // again here duplicates that handoff and causes the original request to be
        // rejected even though PAM accepted the password.
        Ok(())
    }

    async fn cancel_authentication(&self, cookie: String) {
        // Session cancellation from polkitd's side (e.g. the requesting app gave
        // up) isn't wired to the in-flight glib-thread Session yet; the dialog
        // will simply time out or the user will cancel it manually. Tracking
        // in-flight sessions by cookie to cancel them here is a follow-up.
        flog_warn!("polkit CancelAuthentication for cookie {cookie} (not yet actioned)");
    }
}

/// Extracts the `uid` (u32) from a `unix-user` identity's details, plus the
/// `"unix-user:<uid>"` string form `polkit::Identity::from_string` expects.
fn unix_user_uid(details: &HashMap<String, OwnedValue>) -> Option<(u32, String)> {
    let uid = u32::try_from(details.get("uid")?.clone()).ok()?;
    Some((uid, format!("unix-user:{uid}")))
}

/// Drives a `polkit_agent::Session` for `identity_str`/`cookie` on the dedicated GLib-loop
/// thread (GObjects aren't thread-safe; `spawn_from_within` hands the future to that thread
/// without requiring the future itself — which owns the `Session` — to be `Send`).
/// Prompts are relayed to `focaldesk-dialogd` via the existing dialog IPC; that call blocks the
/// GLib thread until the user answers, which is fine since nothing else needs that thread's main
/// loop mid-conversation (the helper subprocess behind `Session` is itself waiting on us).
async fn run_agent_session(
    identity_str: String,
    cookie: String,
    message: String,
    icon_name: String,
) -> Result<bool> {
    flog_info!("polkit agent session: dispatching to glib thread, identity={identity_str}");
    let outcome = glib::MainContext::default()
        .spawn_from_within(move || async move {
            flog_info!("polkit agent session: entered glib thread, identity={identity_str}");

            let Ok(Some(identity)) = polkit::Identity::from_string(&identity_str) else {
                flog_error!("failed to parse polkit identity {identity_str}");
                return false;
            };
            flog_info!("polkit agent session: identity parsed, constructing Session");

            let session = polkit_agent::Session::new(&identity, &cookie);
            let (done_tx, done_rx) = futures_channel::oneshot::channel::<bool>();
            let done_tx = Rc::new(RefCell::new(Some(done_tx)));

            session.connect_show_info(|_, text| flog_info!("polkit: {text}"));
            session.connect_show_error(|_, text| flog_warn!("polkit: {text}"));

            {
                let message = message.clone();
                let icon_name = icon_name.clone();
                session.connect_request(move |session, prompt, echo_on| {
                    flog_info!("polkit agent session: request signal fired, prompt={prompt:?} echo_on={echo_on}");
                    let request_id = next_request_id();
                    let response = focaldesk_ipc::dialog::send_dialog_request(
                        &DialogIpcRequest::PolkitAuthPrompt {
                            request_id,
                            message: message.clone(),
                            icon_name: icon_name.clone(),
                            prompt: prompt.to_string(),
                            echo_on,
                        },
                    );
                    flog_info!(
                        "polkit agent session: dialog IPC returned an {}",
                        match &response {
                            Ok(DialogIpcResponse::PolkitAuthAnswer {
                                answer: Some(_), ..
                            }) => "answer",
                            Ok(DialogIpcResponse::PolkitAuthAnswer { answer: None, .. }) =>
                                "empty answer",
                            Ok(_) => "unexpected response",
                            Err(_) => "error",
                        }
                    );
                    match response {
                        Ok(DialogIpcResponse::PolkitAuthAnswer {
                            answer: Some(text), ..
                        }) => session.response(&text),
                        _ => session.cancel(),
                    }
                });
            }

            {
                let done_tx = done_tx.clone();
                session.connect_completed(move |_session, gained_authorization| {
                    flog_info!("polkit agent session: completed signal fired, gained_authorization={gained_authorization}");
                    if let Some(tx) = done_tx.borrow_mut().take() {
                        let _ = tx.send(gained_authorization);
                    }
                });
            }

            flog_info!("polkit agent session: calling Session::initiate()");
            session.initiate();
            flog_info!("polkit agent session: initiate() returned, awaiting completion");
            let result = done_rx.await.unwrap_or(false);
            flog_info!("polkit agent session: done_rx resolved, gained_authorization={result}");
            result
        })
        .await
        .map_err(|err| anyhow::anyhow!("polkit agent session task failed: {err}"));
    flog_info!("polkit agent session: spawn_from_within returned {outcome:?}");
    outcome
}

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Build the `unix-session` Subject for RegisterAuthenticationAgent/UnregisterAuthenticationAgent.
///
/// Resolve the exact logind session containing this process. The compositor launches the agent
/// directly, so it inherits the graphical session's cgroup. Selecting a session by uid/seat is
/// unsafe because logind can retain an older closing session on the same seat.
async fn session_subject(connection: &Connection) -> Result<Subject> {
    // Prefer the exact ID inherited from the graphical session. pam_systemd
    // does not always export it (notably when a user manager already exists),
    // so fall back to logind's seat-bound session for our uid. Do not use
    // GetSessionByPID: Fedora's system-bus policy denies it to this process.
    let session_id = match std::env::var("XDG_SESSION_ID") {
        Ok(session_id) if !session_id.is_empty() => session_id,
        _ => {
            let manager = Login1ManagerProxy::new(connection)
                .await
                .context("build login1 Manager proxy")?;
            let uid = nix::unistd::Uid::current().as_raw();
            manager
                .list_sessions()
                .await
                .context("list login sessions")?
                .into_iter()
                .find(|(_, session_uid, _, seat, _)| *session_uid == uid && !seat.is_empty())
                .map(|(session_id, ..)| session_id)
                .with_context(|| format!("no seat-bound logind session found for uid {uid}"))?
        }
    };
    let mut subject_details = HashMap::new();
    subject_details.insert("session-id".to_string(), Value::from(session_id).into());
    Ok(Subject {
        subject_kind: "unix-session".to_string(),
        subject_details,
    })
}

async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(err) => {
            flog_error!("failed to install SIGTERM handler: {err}; falling back to Ctrl-C only");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };

    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

/// Runs the default GLib main context/loop on its own thread for the lifetime of the process.
/// `polkit_agent::Session` is signal/GObject-based and must live on a thread that's actually
/// pumping this loop; `glib::MainContext::spawn_from_within` (used in `run_agent_session`) is
/// how work gets handed to it from the tokio side.
fn spawn_glib_main_loop() {
    std::thread::Builder::new()
        .name("glib-main-loop".into())
        .spawn(|| {
            glib::MainLoop::new(None, false).run();
        })
        .expect("failed to spawn GLib main loop thread");
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    flog_info!("FocalDesk polkit agent starting...");

    spawn_glib_main_loop();

    let connection = Connection::system()
        .await
        .context("connect to system bus")?;
    connection
        .object_server()
        .at(AGENT_OBJECT_PATH, AuthenticationAgent)
        .await
        .context("export AuthenticationAgent object")?;

    let subject = session_subject(&connection).await?;
    let authority = AuthorityProxy::new(&connection)
        .await
        .context("build Authority proxy")?;
    authority
        .register_authentication_agent(&subject, "en_US.UTF-8", AGENT_OBJECT_PATH)
        .await
        .context("RegisterAuthenticationAgent")?;
    flog_info!("Registered as PolicyKit authentication agent (subject={subject:?})");

    wait_for_shutdown_signal().await;

    match authority
        .unregister_authentication_agent(&subject, AGENT_OBJECT_PATH)
        .await
    {
        Ok(()) => flog_info!("Unregistered PolicyKit authentication agent"),
        Err(err) => flog_warn!("failed to unregister polkit authentication agent: {err}"),
    }

    Ok(())
}
