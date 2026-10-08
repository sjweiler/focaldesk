use anyhow::{Context, Result, anyhow, bail};
use focaldesk_ai::{
    AiIpcRequest, AiIpcResponse, ConnectorStatus, EventFabricSnapshot, EventSource, send_ai_request,
};
use focaldesk_ipc::{
    ConnectorHostIpcRequest, ConnectorHostIpcResponse, ConnectorRuntimeStatus, IpcRequest,
    IpcResponse, NotificationIpcRequest, NotificationIpcResponse, connector_host_socket_path,
    send_desktop_request, send_notification_request, transport,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

const SUPERVISOR_TICK: Duration = Duration::from_secs(1);
const REGISTRY_REFRESH_SECONDS: u64 = 5;
const BUILTIN_POLL_SECONDS: u64 = 15;
const MAX_CONNECTOR_OUTPUT_BYTES: usize = 64 * 1024;
const QUARANTINE_FAILURES: u32 = 5;

#[derive(Default)]
struct ManagedRuntime {
    status: Option<ConnectorRuntimeStatus>,
    last_seen: BTreeSet<String>,
    last_adapter_state: Option<String>,
    adapter_failure_streak: u32,
    force_poll: bool,
}

#[derive(Default)]
struct HostState {
    paused: bool,
    runtimes: BTreeMap<String, ManagedRuntime>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectorOutput {
    source: EventSource,
    payload: Value,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let state = Arc::new(Mutex::new(HostState::default()));
    serve_control_socket(state.clone())?;
    info!("FocalDesk connector host started; all connector consent remains service-owned");

    let mut connectors = Vec::new();
    let mut fabric = None;
    let mut last_refresh = 0;
    loop {
        let now = unix_now();
        if now.saturating_sub(last_refresh) >= REGISTRY_REFRESH_SECONDS {
            match refresh_control_plane() {
                Ok((next_connectors, next_fabric)) => {
                    connectors = next_connectors;
                    fabric = Some(next_fabric);
                    reconcile_state(&state, &connectors);
                    last_refresh = now;
                }
                Err(error) => warn!(%error, "connector host could not refresh AI control plane"),
            }
        }
        if let Some(fabric) = &fabric {
            supervise_once(&state, &connectors, fabric, now);
        }
        thread::sleep(SUPERVISOR_TICK);
    }
}

fn refresh_control_plane() -> Result<(Vec<ConnectorStatus>, EventFabricSnapshot)> {
    let connectors = match send_ai_request(&AiIpcRequest::ListConnectors)? {
        AiIpcResponse::Connectors { connectors } => connectors,
        AiIpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected connector-list response: {other:?}"),
    };
    let fabric = match send_ai_request(&AiIpcRequest::GetEventFabricState)? {
        AiIpcResponse::EventFabricState { state } => state,
        AiIpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected Event Fabric response: {other:?}"),
    };
    Ok((connectors, fabric))
}

fn reconcile_state(state: &Arc<Mutex<HostState>>, connectors: &[ConnectorStatus]) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    let known = connectors
        .iter()
        .map(|connector| connector.manifest.id.as_str())
        .collect::<BTreeSet<_>>();
    state
        .runtimes
        .retain(|connector_id, _| known.contains(connector_id.as_str()));
    for connector in connectors {
        let runtime = state
            .runtimes
            .entry(connector.manifest.id.clone())
            .or_default();
        let status = runtime
            .status
            .get_or_insert_with(|| runtime_status(connector));
        if !connector.enabled {
            status.state = "disabled".into();
            status.next_run_at_unix = None;
            status.quarantined = false;
            status.consecutive_failures = 0;
        } else if status.state == "disabled" {
            status.state = "ready".into();
            status.next_run_at_unix = Some(0);
        }
    }
}

fn supervise_once(
    state: &Arc<Mutex<HostState>>,
    connectors: &[ConnectorStatus],
    fabric: &EventFabricSnapshot,
    now: u64,
) {
    if state.lock().map(|state| state.paused).unwrap_or(true) || !fabric.connected {
        return;
    }
    for connector in connectors.iter().filter(|connector| connector.enabled) {
        if connector.manifest.id == "workflow-events" {
            set_delegated(state, &connector.manifest.id);
            continue;
        }
        if !connector
            .manifest
            .event_sources
            .iter()
            .any(|source| source_enabled(fabric, *source))
        {
            set_waiting_for_consent(state, &connector.manifest.id);
            continue;
        }
        let due = state.lock().is_ok_and(|state| {
            state
                .runtimes
                .get(&connector.manifest.id)
                .is_some_and(|runtime| {
                    let status = runtime.status.as_ref();
                    !status.is_some_and(|status| status.quarantined)
                        && (runtime.force_poll
                            || status
                                .and_then(|status| status.next_run_at_unix)
                                .is_none_or(|next| next <= now))
                })
        });
        if !due {
            continue;
        }
        mark_running(state, &connector.manifest.id);
        let started = Instant::now();
        let result = run_connector(connector, state);
        finish_run(state, connector, now, started.elapsed(), result);
    }
}

fn source_enabled(fabric: &EventFabricSnapshot, source: EventSource) -> bool {
    fabric
        .policies
        .iter()
        .any(|policy| policy.source == source && policy.enabled)
}

fn run_connector(connector: &ConnectorStatus, state: &Arc<Mutex<HostState>>) -> Result<u64> {
    match connector.manifest.id.as_str() {
        "desktop-events" => run_desktop_adapter(connector, state),
        "service-health" => run_service_health_adapter(connector, state),
        "notifications" => run_notification_adapter(connector, state),
        "local-calendar" => run_calendar_adapter(connector, state),
        _ => run_external_connector(connector),
    }
}

fn run_desktop_adapter(connector: &ConnectorStatus, state: &Arc<Mutex<HostState>>) -> Result<u64> {
    let snapshot = match send_desktop_request(&IpcRequest::GetDesktopSnapshot)
        .map_err(|error| anyhow!(error))?
    {
        IpcResponse::DesktopSnapshot { snapshot } => snapshot,
        IpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected desktop response: {other:?}"),
    };
    let focused = snapshot
        .windows
        .iter()
        .find(|window| Some(window.id) == snapshot.session.focused_window_id);
    let key = format!(
        "{}:{}:{}",
        snapshot.session.active_workspace_id,
        focused
            .and_then(|window| window.app_id.as_deref())
            .unwrap_or(""),
        focused.map(|window| window.title.as_str()).unwrap_or("")
    );
    if has_seen(state, &connector.manifest.id, &key) {
        return Ok(0);
    }
    publish_managed(
        connector,
        EventSource::Desktop,
        json!({
            "event": "desktop focus changed",
            "app_id": focused.and_then(|window| window.app_id.clone()),
            "workspace_id": snapshot.session.active_workspace_id,
            "window_title": focused.map(|window| window.title.clone()),
        }),
    )?;
    remember(state, &connector.manifest.id, key);
    Ok(1)
}

fn run_service_health_adapter(
    connector: &ConnectorStatus,
    state: &Arc<Mutex<HostState>>,
) -> Result<u64> {
    match send_desktop_request(&IpcRequest::GetDesktopSnapshot) {
        Ok(IpcResponse::DesktopSnapshot { snapshot }) => {
            set_adapter_failure_streak(state, &connector.manifest.id, 0);
            let health = if snapshot.rendering.compositor_ready {
                "healthy"
            } else {
                "degraded"
            };
            let key = format!(
                "health:{health}:{}:{}",
                snapshot.rendering.backend, snapshot.rendering.output_count
            );
            if adapter_state_is(state, &connector.manifest.id, &key) {
                return Ok(0);
            }
            publish_managed(
                connector,
                EventSource::ServiceHealth,
                json!({
                    "event": "service health check",
                    "service": "focaldesk-desktop",
                    "state": health,
                    "message": format!("backend={} outputs={}", snapshot.rendering.backend, snapshot.rendering.output_count),
                    "failure_count": if snapshot.rendering.compositor_ready { 0 } else { 1 },
                }),
            )?;
            set_adapter_state(state, &connector.manifest.id, key);
            Ok(1)
        }
        Ok(IpcResponse::Error { message }) | Err(message) => {
            let streak = increment_adapter_failure_streak(state, &connector.manifest.id);
            if streak != 3 {
                return Ok(0);
            }
            publish_managed(
                connector,
                EventSource::ServiceHealth,
                json!({
                    "event": "repeated failure",
                    "service": "focaldesk-desktop",
                    "state": "unavailable",
                    "message": message,
                    "failure_count": streak,
                }),
            )?;
            set_adapter_state(state, &connector.manifest.id, "health:unavailable".into());
            Ok(1)
        }
        Ok(other) => bail!("unexpected desktop health response: {other:?}"),
    }
}

fn run_notification_adapter(
    connector: &ConnectorStatus,
    state: &Arc<Mutex<HostState>>,
) -> Result<u64> {
    let notifications = match send_notification_request(&NotificationIpcRequest::GetVisible)
        .map_err(|error| anyhow!(error))?
    {
        NotificationIpcResponse::VisibleNotifications { notifications } => notifications,
        NotificationIpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected notification response: {other:?}"),
    };
    let mut delivered = 0;
    for notification in notifications.into_iter().filter(|item| item.unread) {
        let key = format!("notification:{}", notification.id);
        if has_seen(state, &connector.manifest.id, &key) {
            continue;
        }
        publish_managed(
            connector,
            EventSource::Notification,
            json!({
                "event": "notification received",
                "app_id": "unknown",
                "summary": notification.title,
                "body": notification.body,
                "urgency": "normal",
            }),
        )?;
        remember(state, &connector.manifest.id, key);
        delivered += 1;
    }
    Ok(delivered)
}

fn run_calendar_adapter(connector: &ConnectorStatus, state: &Arc<Mutex<HostState>>) -> Result<u64> {
    let Some(path) = std::env::var_os("FOCALDESK_LOCAL_CALENDAR_ICS").map(PathBuf::from) else {
        return Ok(0);
    };
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("inspect local calendar {}", path.display()))?;
    if !path.is_absolute() || !metadata.file_type().is_file() || metadata.len() > 1024 * 1024 {
        bail!("local calendar must be an absolute regular .ics file no larger than 1 MiB");
    }
    let text = fs::read_to_string(&path)?;
    let mut delivered = 0;
    for event in parse_ics_events(&text) {
        if has_seen(state, &connector.manifest.id, &event.id) {
            continue;
        }
        let key = event.id.clone();
        publish_managed(
            connector,
            EventSource::Calendar,
            json!({
                "event": "calendar meeting",
                "title": event.title,
                "start_time": event.start,
                "end_time": event.end,
                "organizer": event.organizer,
            }),
        )?;
        remember(state, &connector.manifest.id, key);
        delivered += 1;
    }
    Ok(delivered)
}

fn run_external_connector(connector: &ConnectorStatus) -> Result<u64> {
    let runtime = connector
        .manifest
        .runtime
        .as_ref()
        .ok_or_else(|| anyhow!("external connector has no managed runtime declaration"))?;
    validate_executable(&runtime.executable)?;
    let unit = format!(
        "focaldesk-connector-{}-{:08x}",
        connector.manifest.id,
        unix_now() as u32
    );
    let mut command = Command::new("systemd-run");
    command.args([
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        &format!("--unit={unit}"),
        "--property=NoNewPrivileges=yes",
        "--property=PrivateTmp=yes",
        "--property=ProtectSystem=strict",
        "--property=ProtectHome=read-only",
        "--property=RestrictNamespaces=yes",
        "--property=LockPersonality=yes",
        "--property=MemoryDenyWriteExecute=yes",
        "--property=RuntimeMaxSec=60",
        &format!("--property=MemoryMax={}M", runtime.memory_max_mib),
        &format!("--property=CPUQuota={}%", runtime.cpu_quota_percent),
    ]);
    add_network_sandbox(&mut command, connector)?;
    command
        .arg("--")
        .arg(&runtime.executable)
        .args(&runtime.args);
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .context("launch connector transient service")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("connector service output pipe is unavailable"))?;
    let mut bytes = Vec::with_capacity(MAX_CONNECTOR_OUTPUT_BYTES.min(8192));
    stdout
        .by_ref()
        .take(MAX_CONNECTOR_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read connector service output")?;
    if bytes.len() > MAX_CONNECTOR_OUTPUT_BYTES {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
        bail!("connector output exceeds 64 KiB");
    }
    let status = child
        .wait()
        .context("wait for connector transient service")?;
    if !status.success() {
        bail!("connector service failed with status {status}");
    }
    let text = std::str::from_utf8(&bytes).context("connector output is not UTF-8")?;
    let mut delivered = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let event: ConnectorOutput = serde_json::from_str(line)
            .context("connector output line does not match the event protocol")?;
        publish_managed(connector, event.source, event.payload)?;
        delivered += 1;
    }
    Ok(delivered)
}

fn add_network_sandbox(command: &mut Command, connector: &ConnectorStatus) -> Result<()> {
    if !connector.network_allowed || connector.manifest.network_domains.is_empty() {
        command.arg("--property=RestrictAddressFamilies=AF_UNIX");
        return Ok(());
    }
    command.args([
        "--property=RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "--property=IPAddressDeny=any",
    ]);
    let mut addresses = BTreeSet::new();
    for domain in &connector.manifest.network_domains {
        for address in (domain.as_str(), 443)
            .to_socket_addrs()
            .with_context(|| format!("resolve declared connector domain {domain}"))?
        {
            addresses.insert(address.ip());
        }
    }
    if addresses.is_empty() {
        bail!("declared connector domains resolved to no addresses");
    }
    for address in addresses {
        command.arg(format!("--property=IPAddressAllow={address}"));
    }
    Ok(())
}

fn validate_executable(executable: &str) -> Result<()> {
    let path = Path::new(executable);
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect connector executable {}", path.display()))?;
    if !path.is_absolute()
        || !metadata.file_type().is_file()
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        bail!(
            "connector executable must be absolute, regular, executable, and not group/world writable"
        );
    }
    Ok(())
}

fn publish_managed(connector: &ConnectorStatus, source: EventSource, payload: Value) -> Result<()> {
    let response = send_ai_request(&AiIpcRequest::PublishManagedConnectorEvent {
        connector_id: connector.manifest.id.clone(),
        source,
        payload,
    })?;
    match response {
        AiIpcResponse::EventDelivered { .. } => Ok(()),
        AiIpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected managed-event response: {other:?}"),
    }
}

fn has_seen(state: &Arc<Mutex<HostState>>, connector_id: &str, key: &str) -> bool {
    state.lock().is_ok_and(|state| {
        state
            .runtimes
            .get(connector_id)
            .is_some_and(|runtime| runtime.last_seen.contains(key))
    })
}

fn remember(state: &Arc<Mutex<HostState>>, connector_id: &str, key: String) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    let seen = &mut state
        .runtimes
        .entry(connector_id.to_string())
        .or_default()
        .last_seen;
    if seen.len() >= 512
        && let Some(oldest) = seen.first().cloned()
    {
        seen.remove(&oldest);
    }
    seen.insert(key);
}

fn set_adapter_failure_streak(state: &Arc<Mutex<HostState>>, connector_id: &str, streak: u32) {
    if let Ok(mut state) = state.lock() {
        state
            .runtimes
            .entry(connector_id.to_string())
            .or_default()
            .adapter_failure_streak = streak;
    }
}

fn increment_adapter_failure_streak(state: &Arc<Mutex<HostState>>, connector_id: &str) -> u32 {
    let Ok(mut state) = state.lock() else {
        return 0;
    };
    let streak = &mut state
        .runtimes
        .entry(connector_id.to_string())
        .or_default()
        .adapter_failure_streak;
    *streak = streak.saturating_add(1);
    *streak
}

fn adapter_state_is(state: &Arc<Mutex<HostState>>, connector_id: &str, value: &str) -> bool {
    state.lock().is_ok_and(|state| {
        state
            .runtimes
            .get(connector_id)
            .and_then(|runtime| runtime.last_adapter_state.as_deref())
            == Some(value)
    })
}

fn set_adapter_state(state: &Arc<Mutex<HostState>>, connector_id: &str, value: String) {
    if let Ok(mut state) = state.lock() {
        state
            .runtimes
            .entry(connector_id.to_string())
            .or_default()
            .last_adapter_state = Some(value);
    }
}

fn finish_run(
    state: &Arc<Mutex<HostState>>,
    connector: &ConnectorStatus,
    now: u64,
    duration: Duration,
    result: Result<u64>,
) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    let runtime = state
        .runtimes
        .entry(connector.manifest.id.clone())
        .or_default();
    runtime.force_poll = false;
    let status = runtime
        .status
        .get_or_insert_with(|| runtime_status(connector));
    status.last_run_at_unix = Some(now);
    status.last_duration_ms = Some(duration.as_millis().min(u64::MAX as u128) as u64);
    match result {
        Ok(delivered) => {
            status.state = "idle".into();
            status.consecutive_failures = 0;
            status.last_error = None;
            status.delivered_events = status.delivered_events.saturating_add(delivered);
            status.next_run_at_unix = Some(now.saturating_add(poll_interval(connector)));
        }
        Err(error) => {
            status.consecutive_failures = status.consecutive_failures.saturating_add(1);
            status.last_error = Some(error.to_string().chars().take(500).collect());
            status.quarantined = status.consecutive_failures >= QUARANTINE_FAILURES;
            status.state = if status.quarantined {
                "quarantined".into()
            } else {
                "backoff".into()
            };
            let exponent = status.consecutive_failures.min(6);
            let backoff = 5_u64.saturating_mul(1_u64 << exponent).min(300);
            status.next_run_at_unix = (!status.quarantined).then_some(now.saturating_add(backoff));
        }
    }
}

fn poll_interval(connector: &ConnectorStatus) -> u64 {
    connector
        .manifest
        .runtime
        .as_ref()
        .map_or(BUILTIN_POLL_SECONDS, |runtime| {
            runtime.poll_interval_seconds
        })
}

fn runtime_status(connector: &ConnectorStatus) -> ConnectorRuntimeStatus {
    ConnectorRuntimeStatus {
        connector_id: connector.manifest.id.clone(),
        state: if connector.enabled {
            "ready"
        } else {
            "disabled"
        }
        .into(),
        consecutive_failures: 0,
        quarantined: false,
        last_run_at_unix: None,
        next_run_at_unix: connector.enabled.then_some(0),
        last_duration_ms: None,
        last_error: None,
        delivered_events: 0,
    }
}

fn mark_running(state: &Arc<Mutex<HostState>>, connector_id: &str) {
    if let Ok(mut state) = state.lock()
        && let Some(status) = state
            .runtimes
            .get_mut(connector_id)
            .and_then(|runtime| runtime.status.as_mut())
    {
        status.state = "running".into();
    }
}

fn set_delegated(state: &Arc<Mutex<HostState>>, connector_id: &str) {
    if let Ok(mut state) = state.lock()
        && let Some(status) = state
            .runtimes
            .get_mut(connector_id)
            .and_then(|runtime| runtime.status.as_mut())
    {
        status.state = "delegated_to_ai_supervisor".into();
        status.next_run_at_unix = None;
    }
}

fn set_waiting_for_consent(state: &Arc<Mutex<HostState>>, connector_id: &str) {
    if let Ok(mut state) = state.lock()
        && let Some(status) = state
            .runtimes
            .get_mut(connector_id)
            .and_then(|runtime| runtime.status.as_mut())
    {
        status.state = "waiting_for_source_consent".into();
        status.next_run_at_unix = None;
    }
}

#[derive(Default)]
struct IcsEvent {
    id: String,
    title: String,
    start: String,
    end: String,
    organizer: String,
}

fn parse_ics_events(text: &str) -> Vec<IcsEvent> {
    let mut events = Vec::new();
    let mut current = None;
    for line in text.lines().map(str::trim) {
        match line {
            "BEGIN:VEVENT" => current = Some(IcsEvent::default()),
            "END:VEVENT" => {
                if let Some(event) = current.take()
                    && !event.id.is_empty()
                    && !event.title.is_empty()
                {
                    events.push(event);
                }
            }
            _ => {
                let Some(event) = current.as_mut() else {
                    continue;
                };
                if let Some(value) = property_value(line, "UID") {
                    event.id = value.into();
                } else if let Some(value) = property_value(line, "SUMMARY") {
                    event.title = value.into();
                } else if let Some(value) = property_value(line, "DTSTART") {
                    event.start = value.into();
                } else if let Some(value) = property_value(line, "DTEND") {
                    event.end = value.into();
                } else if let Some(value) = property_value(line, "ORGANIZER") {
                    event.organizer = value.into();
                }
            }
        }
    }
    events
}

fn property_value<'a>(line: &'a str, property: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    (key == property || key.starts_with(&format!("{property};"))).then_some(value)
}

fn serve_control_socket(state: Arc<Mutex<HostState>>) -> Result<()> {
    let path = connector_host_socket_path().map_err(|error| anyhow!(error))?;
    let listener = transport::bind_user_socket(&path)
        .with_context(|| format!("bind connector-host IPC socket {}", path.display()))?;
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            handle_control_client(&mut stream, &state);
        }
    });
    Ok(())
}

fn handle_control_client(stream: &mut UnixStream, state: &Arc<Mutex<HostState>>) {
    if transport::require_authorized_peer(stream, transport::CONNECTOR_HOST_POLICY).is_err() {
        return;
    }
    let response = transport::read_limited(stream)
        .and_then(|bytes| {
            transport::decode_message::<ConnectorHostIpcRequest>(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .map(|request| apply_control_request(state, request))
        .unwrap_or_else(|error| ConnectorHostIpcResponse {
            status: "error".into(),
            message: Some(error.to_string()),
            paused: state.lock().map(|state| state.paused).unwrap_or(true),
            connectors: Vec::new(),
        });
    if let Ok(encoded) = transport::encode_message(&response) {
        let _ = stream.write_all(&encoded);
    }
}

fn apply_control_request(
    state: &Arc<Mutex<HostState>>,
    request: ConnectorHostIpcRequest,
) -> ConnectorHostIpcResponse {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(_) => {
            return ConnectorHostIpcResponse {
                status: "error".into(),
                message: Some("connector host state unavailable".into()),
                paused: true,
                connectors: Vec::new(),
            };
        }
    };
    let mut message = None;
    match request {
        ConnectorHostIpcRequest::Status => {}
        ConnectorHostIpcRequest::SetPaused { paused } => state.paused = paused,
        ConnectorHostIpcRequest::PollNow { connector_id } => {
            for (id, runtime) in &mut state.runtimes {
                if connector_id
                    .as_ref()
                    .is_none_or(|requested| requested == id)
                {
                    runtime.force_poll = true;
                    if let Some(status) = runtime.status.as_mut()
                        && !status.quarantined
                    {
                        status.next_run_at_unix = Some(0);
                    }
                }
            }
        }
        ConnectorHostIpcRequest::ClearQuarantine { connector_id } => {
            if let Some(runtime) = state.runtimes.get_mut(&connector_id) {
                if let Some(status) = runtime.status.as_mut() {
                    status.quarantined = false;
                    status.consecutive_failures = 0;
                    status.state = "ready".into();
                    status.next_run_at_unix = Some(0);
                    status.last_error = None;
                }
            } else {
                message = Some(format!("unknown connector runtime: {connector_id}"));
            }
        }
    }
    ConnectorHostIpcResponse {
        status: if message.is_some() { "error" } else { "ok" }.into(),
        message,
        paused: state.paused,
        connectors: state
            .runtimes
            .values()
            .filter_map(|runtime| runtime.status.clone())
            .collect(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_fixture_parser_is_bounded_to_named_fields() {
        let events = parse_ics_events(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:one\nSUMMARY:Team meeting\nDTSTART:20261008T130000Z\nDTEND:20261008T140000Z\nORGANIZER:mailto:test@example.test\nDESCRIPTION:must not be captured\nEND:VEVENT\nEND:VCALENDAR",
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Team meeting");
        assert_eq!(events[0].start, "20261008T130000Z");
    }

    #[test]
    fn failures_quarantine_after_bounded_retries() {
        let connector = ConnectorStatus {
            manifest: focaldesk_ai::built_in_connectors()
                .into_iter()
                .find(|manifest| manifest.id == "service-health")
                .unwrap(),
            enabled: true,
            network_allowed: false,
            health: focaldesk_ai::ConnectorHealth::Ready,
            last_event_at_unix: None,
            last_error: None,
            rollback_available: false,
        };
        let state = Arc::new(Mutex::new(HostState::default()));
        reconcile_state(&state, std::slice::from_ref(&connector));
        for _ in 0..QUARANTINE_FAILURES {
            finish_run(
                &state,
                &connector,
                unix_now(),
                Duration::from_millis(1),
                Err(anyhow!("fixture failure")),
            );
        }
        assert!(
            state.lock().unwrap().runtimes["service-health"]
                .status
                .as_ref()
                .unwrap()
                .quarantined
        );
    }
}
