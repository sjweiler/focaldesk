use anyhow::{Context, Result};
use focaldesk_ipc::dialog::{DialogIpcRequest, DialogIpcResponse};
use focaldesk_logging::{flog_error, flog_info, flog_warn};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{dbus_interface, dbus_proxy, Connection, DBusError};
use zbus_polkit::policykit1::{AuthorityProxy, Identity as WireIdentity, Subject};

const AGENT_OBJECT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";

#[dbus_proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    // zbus's snake_case->CamelCase auto-derivation doesn't know "pid" is an
    // acronym and produces "GetSessionByPid"; the real D-Bus method (and the
    // dbus-daemon policy rule allowing it) is "GetSessionByPID". Getting this
    // wrong doesn't fail to compile — it fails at runtime as a confusing
    // AccessDenied, because the mismatched name just falls through to the
    // policy's default deny rule instead of matching the intended allow rule.
    #[dbus_proxy(name = "GetSessionByPID")]
    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
}

#[dbus_proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    #[dbus_proxy(property)]
    fn id(&self) -> zbus::Result<String>;
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

struct AuthenticationAgent {
    connection: Connection,
}

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
        let Some((uid, identity_str)) = identities
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

        let authority = AuthorityProxy::new(&self.connection)
            .await
            .map_err(AgentError::ZBus)?;
        let mut identity_details = HashMap::new();
        identity_details.insert("uid", Value::from(uid));
        let identity = WireIdentity {
            identity_kind: "unix-user",
            identity_details: &identity_details,
        };
        authority
            .authentication_agent_response2(uid, &cookie, &identity)
            .await
            .map_err(AgentError::ZBus)?;

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
    glib::MainContext::default()
        .spawn_from_within(move || async move {
            let Ok(Some(identity)) = polkit::Identity::from_string(&identity_str) else {
                flog_error!("failed to parse polkit identity {identity_str}");
                return false;
            };

            let session = polkit_agent::Session::new(&identity, &cookie);
            let (done_tx, done_rx) = futures_channel::oneshot::channel::<bool>();
            let done_tx = Rc::new(RefCell::new(Some(done_tx)));

            session.connect_show_info(|_, text| flog_info!("polkit: {text}"));
            session.connect_show_error(|_, text| flog_warn!("polkit: {text}"));

            {
                let message = message.clone();
                let icon_name = icon_name.clone();
                session.connect_request(move |session, prompt, echo_on| {
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
                    if let Some(tx) = done_tx.borrow_mut().take() {
                        let _ = tx.send(gained_authorization);
                    }
                });
            }

            session.initiate();
            done_rx.await.unwrap_or(false)
        })
        .await
        .map_err(|err| anyhow::anyhow!("polkit agent session task failed: {err}"))
}

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Build the `unix-session` Subject for RegisterAuthenticationAgent/UnregisterAuthenticationAgent,
/// looked up from logind rather than trusting `$XDG_SESSION_ID` (not guaranteed set everywhere).
async fn session_subject(connection: &Connection) -> Result<Subject> {
    let manager = Login1ManagerProxy::new(connection)
        .await
        .context("build login1 Manager proxy")?;
    let pid = std::process::id();
    let session_path = manager
        .get_session_by_pid(pid)
        .await
        .context("GetSessionByPID")?;

    let session = Login1SessionProxy::builder(connection)
        .path(session_path.as_ref())
        .context("set login1 Session proxy path")?
        .build()
        .await
        .context("build login1 Session proxy")?;
    let session_id = session.id().await.context("read login1 session id")?;

    let mut subject_details = HashMap::new();
    subject_details.insert("session-id".to_string(), Value::from(session_id).into());
    Ok(Subject {
        subject_kind: "unix-session".to_string(),
        subject_details,
    })
}

async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

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
        .at(
            AGENT_OBJECT_PATH,
            AuthenticationAgent {
                connection: connection.clone(),
            },
        )
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
