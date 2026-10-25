use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use focaldesk_ai::{
    AiIpcRequest, AiIpcResponse, ConnectorStatus, EventFabricSnapshot, EventSource, send_ai_request,
};
use focaldesk_calendar::{CalendarEvent, CalendarProvider, CalendarStore, SyncBatch};
use focaldesk_ipc::{
    CalendarAccountSnapshot, CalendarAvailableSnapshot, CalendarEventSnapshot,
    CalendarOAuthSnapshot, ConnectorHostIpcRequest, ConnectorHostIpcResponse,
    ConnectorRuntimeStatus, IpcRequest, IpcResponse, NotificationIpcRequest,
    NotificationIpcResponse, connector_host_socket_path, send_desktop_request,
    send_notification_request, transport,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::ToSocketAddrs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use zeroize::Zeroizing;

const SUPERVISOR_TICK: Duration = Duration::from_secs(1);
const REGISTRY_REFRESH_SECONDS: u64 = 5;
const BUILTIN_POLL_SECONDS: u64 = 15;
const MAX_CONNECTOR_OUTPUT_BYTES: usize = 64 * 1024;
const QUARANTINE_FAILURES: u32 = 5;
static CALENDAR_STORE: OnceLock<Arc<CalendarStore>> = OnceLock::new();

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
    oauth_flows: BTreeMap<String, CalendarOAuthSnapshot>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectorOutput {
    source: EventSource,
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalendarAccountsDocument {
    version: u16,
    #[serde(default)]
    accounts: Vec<CalendarAccount>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CalendarAccount {
    id: String,
    provider: CalendarProvider,
    #[serde(default)]
    enabled: bool,
    client_id: String,
    #[serde(default = "primary_calendar")]
    calendar_id: String,
    #[serde(default)]
    calendar_ids: Vec<String>,
    #[serde(default = "default_reminder_minutes")]
    reminder_minutes: u32,
    #[serde(default = "common_tenant")]
    tenant: String,
}

fn primary_calendar() -> String {
    "primary".into()
}
const fn default_reminder_minutes() -> u32 {
    10
}

fn selected_calendar_ids(account: &CalendarAccount) -> Vec<String> {
    if account.calendar_ids.is_empty() {
        vec![account.calendar_id.clone()]
    } else {
        account.calendar_ids.clone()
    }
}
fn common_tenant() -> String {
    "common".into()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let calendar = Arc::new(CalendarStore::open(calendar_state_path())?);
    CALENDAR_STORE
        .set(calendar)
        .map_err(|_| anyhow!("calendar store was initialized more than once"))?;
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
        dispatch_due_reminders(now);
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
        "google-calendar" => run_cloud_calendar(connector, CalendarProvider::Google),
        "microsoft-calendar" => run_cloud_calendar(connector, CalendarProvider::Microsoft),
        _ => run_external_connector(connector),
    }
}

fn run_cloud_calendar(connector: &ConnectorStatus, provider: CalendarProvider) -> Result<u64> {
    if !connector.network_allowed {
        bail!("calendar provider network access has not been granted");
    }
    let document = load_calendar_accounts()?;
    let accounts = document
        .accounts
        .into_iter()
        .filter(|account| account.enabled && account.provider == provider);
    let http = focaldesk_calendar::CalendarHttpClient::new()?;
    let token_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let store = CALENDAR_STORE.get().context("calendar store unavailable")?;
    let mut delivered = 0;
    for account in accounts {
        validate_account(&account)?;
        let refresh_handle = format!("calendar/{}/refresh-token", account.id);
        let refresh_token = focaldesk_secrets_client::get(&refresh_handle)?;
        let tokens = refresh_access_token(&token_client, &account, &refresh_token)?;
        if let Some(rotated) = tokens.refresh_token.as_deref()
            && rotated != refresh_token.as_str()
        {
            focaldesk_secrets_client::set(
                &refresh_handle,
                rotated,
                &format!("{} calendar refresh token", account.id),
            )?;
        }
        let access_token = Zeroizing::new(tokens.access_token);
        for calendar_id in selected_calendar_ids(&account) {
            // Periodic full sync rolls the bounded provider window forward and
            // recovers from changes that invalidate an opaque provider cursor.
            let cursor =
                store.fresh_cursor(&account.id, &calendar_id, unix_now() as i64, 7 * 86_400)?;
            let batch = match provider {
                CalendarProvider::Google => match http.sync_google(
                    &account.id,
                    &calendar_id,
                    &access_token,
                    cursor.as_deref(),
                ) {
                    Err(error) if error.to_string().contains("google_sync_cursor_expired") => {
                        store.clear_calendar(&account.id, &calendar_id)?;
                        http.sync_google(&account.id, &calendar_id, &access_token, None)?
                    }
                    result => result?,
                },
                CalendarProvider::Microsoft => {
                    let now = chrono::Utc::now();
                    let start = (now - chrono::Duration::days(30))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    let end = (now + chrono::Duration::days(365))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    http.sync_microsoft(
                        &account.id,
                        &calendar_id,
                        &access_token,
                        cursor.as_deref(),
                        &start,
                        &end,
                    )?
                }
                CalendarProvider::Local => unreachable!(),
            };
            for event in &batch.events {
                publish_managed(
                    connector,
                    EventSource::Calendar,
                    json!({
                        "event": "calendar meeting", "title": event.title,
                        "start_time": event.start_time, "end_time": event.end_time,
                        "organizer": event.organizer,
                    }),
                )?;
                delivered += 1;
            }
            store.apply_sync(&account.id, &calendar_id, &batch)?;
        }
    }
    Ok(delivered)
}

fn calendar_accounts_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("focaldesk/calendar-accounts.json")
}

fn save_calendar_account(account: CalendarAccount) -> Result<()> {
    let path = calendar_accounts_path();
    let mut document = load_calendar_accounts()?;
    document.accounts.retain(|item| item.id != account.id);
    document.accounts.push(account);
    let parent = path
        .parent()
        .context("calendar account path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.new");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&json!({"version": 1, "accounts": document.accounts}))?,
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn calendar_account_snapshots(
    state: &Arc<Mutex<HostState>>,
) -> Result<Vec<CalendarAccountSnapshot>> {
    let document = load_calendar_accounts()?;
    let locked = state
        .lock()
        .map_err(|_| anyhow!("connector state unavailable"))?;
    let store = CALENDAR_STORE.get().context("calendar store unavailable")?;
    document
        .accounts
        .into_iter()
        .map(|account| {
            let connector_id = match account.provider {
                CalendarProvider::Google => "google-calendar",
                CalendarProvider::Microsoft => "microsoft-calendar",
                CalendarProvider::Local => "local-calendar",
            };
            let runtime = locked
                .runtimes
                .get(connector_id)
                .and_then(|item| item.status.as_ref());
            Ok(CalendarAccountSnapshot {
                account_id: account.id.clone(),
                provider: account.provider.as_str().into(),
                connected: focaldesk_secrets_client::get(&format!(
                    "calendar/{}/refresh-token",
                    account.id
                ))
                .is_ok(),
                enabled: account.enabled,
                cached_events: store.account_event_count(&account.id)?,
                sync_state: runtime
                    .map(|item| item.state.clone())
                    .unwrap_or_else(|| "connector unavailable".into()),
                calendar_ids: selected_calendar_ids(&account),
                reminder_minutes: account.reminder_minutes,
                last_sync_at_unix: runtime.and_then(|item| item.last_run_at_unix),
                last_error: runtime.and_then(|item| item.last_error.clone()),
            })
        })
        .collect()
}

fn disconnect_calendar_account(account_id: &str, delete_local_data: bool) -> Result<()> {
    let mut document = load_calendar_accounts()?;
    let account = document
        .accounts
        .iter_mut()
        .find(|item| item.id == account_id)
        .context("unknown calendar account")?;
    account.enabled = false;
    save_calendar_account(account.clone())?;
    focaldesk_secrets_client::delete(&format!("calendar/{account_id}/refresh-token"))?;
    let _ = focaldesk_secrets_client::delete(&format!("calendar/{account_id}/client-secret"));
    if delete_local_data {
        CALENDAR_STORE
            .get()
            .context("calendar store unavailable")?
            .clear_account(account_id)?;
    }
    Ok(())
}

fn update_calendar_preferences(
    account_id: &str,
    calendar_ids: Vec<String>,
    reminder_minutes: u32,
) -> Result<()> {
    let mut document = load_calendar_accounts()?;
    let account = document
        .accounts
        .iter_mut()
        .find(|item| item.id == account_id)
        .context("unknown calendar account")?;
    let previous = selected_calendar_ids(account)
        .into_iter()
        .collect::<BTreeSet<_>>();
    account.calendar_ids = calendar_ids;
    account.reminder_minutes = reminder_minutes;
    validate_account(account)?;
    let next = selected_calendar_ids(account)
        .into_iter()
        .collect::<BTreeSet<_>>();
    save_calendar_account(account.clone())?;
    let store = CALENDAR_STORE.get().context("calendar store unavailable")?;
    for removed in previous.difference(&next) {
        store.clear_calendar(account_id, removed)?;
    }
    Ok(())
}

fn calendar_access_token(account: &CalendarAccount) -> Result<Zeroizing<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let handle = format!("calendar/{}/refresh-token", account.id);
    let refresh = focaldesk_secrets_client::get(&handle)?;
    let tokens = refresh_access_token(&client, account, &refresh)?;
    if let Some(rotated) = tokens.refresh_token.as_deref()
        && rotated != refresh.as_str()
    {
        focaldesk_secrets_client::set(
            &handle,
            rotated,
            &format!("{} calendar refresh token", account.id),
        )?;
    }
    Ok(Zeroizing::new(tokens.access_token))
}

fn discover_calendars(account_id: &str) -> Result<Vec<CalendarAvailableSnapshot>> {
    let document = load_calendar_accounts()?;
    let account = document
        .accounts
        .into_iter()
        .find(|item| item.id == account_id)
        .context("unknown calendar account")?;
    validate_account(&account)?;
    let access = calendar_access_token(&account)?;
    let selected = selected_calendar_ids(&account)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut calendars = Vec::new();
    let mut next = match account.provider {
        CalendarProvider::Google => Some(
            "https://www.googleapis.com/calendar/v3/users/me/calendarList?maxResults=250"
                .to_string(),
        ),
        CalendarProvider::Microsoft => Some(
            "https://graph.microsoft.com/v1.0/me/calendars?$select=id,name,isDefaultCalendar"
                .to_string(),
        ),
        CalendarProvider::Local => bail!("local calendars are not discovered through OAuth"),
    };
    for _ in 0..20 {
        let Some(url) = next.take() else { break };
        let parsed = url::Url::parse(&url)?;
        let allowed_host = match account.provider {
            CalendarProvider::Google => "www.googleapis.com",
            CalendarProvider::Microsoft => "graph.microsoft.com",
            CalendarProvider::Local => unreachable!(),
        };
        if parsed.scheme() != "https" || parsed.host_str() != Some(allowed_host) {
            bail!("calendar discovery pagination escaped the provider host");
        }
        let body = client
            .get(parsed)
            .bearer_auth(access.as_str())
            .send()?
            .error_for_status()?
            .json::<Value>()?;
        let items = body
            .get("items")
            .or_else(|| body.get("value"))
            .and_then(Value::as_array)
            .context("calendar provider returned no calendar list")?;
        for item in items {
            let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() || id.len() > 512 || calendars.len() >= 500 {
                continue;
            }
            let name = item
                .get("summary")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("Calendar");
            calendars.push(CalendarAvailableSnapshot {
                id: id.into(),
                name: name.chars().take(200).collect(),
                primary: item
                    .get("primary")
                    .or_else(|| item.get("isDefaultCalendar"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                selected: selected.contains(id)
                    || (selected.contains("primary")
                        && item
                            .get("primary")
                            .or_else(|| item.get("isDefaultCalendar"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false)),
            });
        }
        next = body
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(|token| format!("https://www.googleapis.com/calendar/v3/users/me/calendarList?maxResults=250&pageToken={}", url::form_urlencoded::byte_serialize(token.as_bytes()).collect::<String>()))
            .or_else(|| body.get("@odata.nextLink").and_then(Value::as_str).map(str::to_owned));
    }
    calendars.sort_by_key(|calendar| (!calendar.primary, calendar.name.to_ascii_lowercase()));
    Ok(calendars)
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn set_oauth_state(
    state: &Arc<Mutex<HostState>>,
    flow_id: &str,
    next: &str,
    message: Option<String>,
) {
    if let Ok(mut state) = state.lock()
        && let Some(flow) = state.oauth_flows.get_mut(flow_id)
    {
        flow.state = next.into();
        flow.message = message;
    }
}

fn start_calendar_oauth(
    state: &Arc<Mutex<HostState>>,
    provider: String,
    account_id: String,
    client_id: String,
    client_secret: Option<String>,
    tenant: Option<String>,
) -> Result<CalendarOAuthSnapshot> {
    let provider_kind = match provider.as_str() {
        "google" => CalendarProvider::Google,
        "microsoft" => CalendarProvider::Microsoft,
        _ => bail!("unsupported calendar provider"),
    };
    let account = CalendarAccount {
        id: account_id,
        provider: provider_kind,
        enabled: true,
        client_id,
        calendar_id: "primary".into(),
        calendar_ids: vec!["primary".into()],
        reminder_minutes: default_reminder_minutes(),
        tenant: tenant.unwrap_or_else(common_tenant),
    };
    validate_account(&account)?;
    let flow_id = random_urlsafe(18);
    if provider_kind == CalendarProvider::Google {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let verifier = random_urlsafe(48);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let csrf = random_urlsafe(24);
        let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
        url.query_pairs_mut()
            .append_pair("client_id", &account.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", "https://www.googleapis.com/auth/calendar.readonly")
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &csrf);
        let snapshot = CalendarOAuthSnapshot {
            flow_id: flow_id.clone(),
            provider,
            state: "waiting_for_browser".into(),
            verification_uri: Some(url.into()),
            user_code: None,
            message: None,
        };
        state
            .lock()
            .map_err(|_| anyhow!("connector state unavailable"))?
            .oauth_flows
            .insert(flow_id.clone(), snapshot.clone());
        let state = state.clone();
        thread::spawn(move || {
            let result = complete_google_oauth(
                listener,
                &account,
                client_secret.as_deref(),
                &redirect_uri,
                &verifier,
                &csrf,
            );
            match result {
                Ok(()) => set_oauth_state(
                    &state,
                    &flow_id,
                    "connected",
                    Some("Google Calendar connected".into()),
                ),
                Err(error) => set_oauth_state(&state, &flow_id, "error", Some(error.to_string())),
            }
        });
        Ok(snapshot)
    } else {
        let endpoint = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            account.tenant
        );
        #[derive(Deserialize)]
        struct Device {
            device_code: String,
            user_code: String,
            verification_uri: String,
            expires_in: u64,
            interval: u64,
        }
        let device: Device = reqwest::blocking::Client::new()
            .post(endpoint)
            .form(&[
                ("client_id", account.client_id.as_str()),
                ("scope", "offline_access Calendars.Read"),
            ])
            .send()?
            .error_for_status()?
            .json()?;
        let snapshot = CalendarOAuthSnapshot {
            flow_id: flow_id.clone(),
            provider,
            state: "waiting_for_user".into(),
            verification_uri: Some(device.verification_uri),
            user_code: Some(device.user_code),
            message: Some("Enter the code in your browser".into()),
        };
        state
            .lock()
            .map_err(|_| anyhow!("connector state unavailable"))?
            .oauth_flows
            .insert(flow_id.clone(), snapshot.clone());
        let state = state.clone();
        thread::spawn(move || {
            let result = complete_microsoft_oauth(
                &account,
                &device.device_code,
                device.interval,
                device.expires_in,
            );
            match result {
                Ok(()) => set_oauth_state(
                    &state,
                    &flow_id,
                    "connected",
                    Some("Microsoft 365 Calendar connected".into()),
                ),
                Err(error) => set_oauth_state(&state, &flow_id, "error", Some(error.to_string())),
            }
        });
        Ok(snapshot)
    }
}

fn complete_google_oauth(
    listener: TcpListener,
    account: &CalendarAccount,
    client_secret: Option<&str>,
    redirect_uri: &str,
    verifier: &str,
    csrf: &str,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(10 * 60);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("Google authorization expired");
                }
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(error.into()),
        }
    };
    let mut bytes = vec![0_u8; 8192];
    let count = stream.read(&mut bytes)?;
    let first = std::str::from_utf8(&bytes[..count])?
        .lines()
        .next()
        .context("invalid OAuth callback")?;
    let target = first
        .split_whitespace()
        .nth(1)
        .context("invalid OAuth callback target")?;
    let callback = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
    let values = callback.query_pairs().collect::<BTreeMap<_, _>>();
    if values.get("state").map(|v| v.as_ref()) != Some(csrf) {
        bail!("OAuth state mismatch");
    }
    let code = values
        .get("code")
        .context("Google authorization was declined")?;
    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<h1>FocalDesk connected</h1><p>You can close this tab.</p>";
    stream.write_all(html.as_bytes())?;
    let client = reqwest::blocking::Client::new();
    let mut form = vec![
        ("client_id", account.client_id.as_str()),
        ("code", code.as_ref()),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    let token: TokenResponse = client
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()?
        .error_for_status()?
        .json()?;
    let refresh = token
        .refresh_token
        .context("Google returned no refresh token")?;
    focaldesk_secrets_client::set(
        &format!("calendar/{}/refresh-token", account.id),
        &refresh,
        "Google Calendar refresh token",
    )?;
    if let Some(secret) = client_secret {
        focaldesk_secrets_client::set(
            &format!("calendar/{}/client-secret", account.id),
            secret,
            "Google OAuth client secret",
        )?;
    }
    save_calendar_account(account.clone())
}

fn complete_microsoft_oauth(
    account: &CalendarAccount,
    device_code: &str,
    interval: u64,
    expires: u64,
) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        account.tenant
    );
    let deadline = Instant::now() + Duration::from_secs(expires.min(1800));
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(interval.max(5)));
        let response = client
            .post(&endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", account.client_id.as_str()),
                ("device_code", device_code),
            ])
            .send()?;
        let status = response.status();
        let body: Value = response.json()?;
        if status.is_success() {
            let refresh = body
                .get("refresh_token")
                .and_then(Value::as_str)
                .context("Microsoft returned no refresh token")?;
            focaldesk_secrets_client::set(
                &format!("calendar/{}/refresh-token", account.id),
                refresh,
                "Microsoft Calendar refresh token",
            )?;
            return save_calendar_account(account.clone());
        }
        match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                thread::sleep(Duration::from_secs(5));
                continue;
            }
            Some(error) => bail!("Microsoft authorization failed: {error}"),
            None => bail!("Microsoft authorization failed"),
        }
    }
    bail!("Microsoft authorization expired")
}

fn load_calendar_accounts() -> Result<CalendarAccountsDocument> {
    let path = calendar_accounts_path();
    if !path.exists() {
        return Ok(CalendarAccountsDocument {
            version: 1,
            accounts: Vec::new(),
        });
    }
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.len() > 128 * 1024 {
        bail!("calendar account configuration must be a regular file no larger than 128 KiB");
    }
    let document: CalendarAccountsDocument = serde_json::from_slice(&fs::read(&path)?)?;
    if document.version != 1 {
        bail!("unsupported calendar account configuration version");
    }
    Ok(document)
}

fn validate_account(account: &CalendarAccount) -> Result<()> {
    let calendar_ids = selected_calendar_ids(account);
    let unique_calendar_ids = calendar_ids.iter().collect::<BTreeSet<_>>();
    let valid_id = !account.id.is_empty()
        && account.id.len() <= 64
        && account
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if !valid_id
        || account.client_id.trim().is_empty()
        || account.client_id.len() > 256
        || calendar_ids.is_empty()
        || calendar_ids.len() > 32
        || unique_calendar_ids.len() != calendar_ids.len()
        || calendar_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 512)
        || !matches!(account.reminder_minutes, 0 | 5 | 10 | 15 | 30 | 60)
    {
        bail!("calendar account configuration is invalid");
    }
    if account.tenant.is_empty()
        || account.tenant.len() > 128
        || !account
            .tenant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    {
        bail!("calendar account tenant is invalid");
    }
    Ok(())
}

fn refresh_access_token(
    client: &reqwest::blocking::Client,
    account: &CalendarAccount,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let response = match account.provider {
        CalendarProvider::Google => {
            let secret_handle = format!("calendar/{}/client-secret", account.id);
            let client_secret = focaldesk_secrets_client::get(&secret_handle).ok();
            let mut form = vec![
                ("client_id", account.client_id.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ];
            if let Some(secret) = client_secret.as_deref() {
                form.push(("client_secret", secret));
            }
            client
                .post("https://oauth2.googleapis.com/token")
                .form(&form)
                .send()?
        }
        CalendarProvider::Microsoft => {
            let endpoint = format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                account.tenant
            );
            client
                .post(endpoint)
                .form(&[
                    ("client_id", account.client_id.as_str()),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                    ("scope", "offline_access Calendars.Read"),
                ])
                .send()?
        }
        CalendarProvider::Local => bail!("local calendars do not use OAuth"),
    };
    Ok(response.error_for_status()?.json()?)
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
    let events = parse_ics_events(&text);
    let normalized = events
        .iter()
        .filter_map(|event| match normalize_ics_event(event) {
            Ok(event) => Some(event),
            Err(error) => {
                warn!(%error, event_id = %event.id, "skipping unsupported local calendar event");
                None
            }
        })
        .collect::<Vec<_>>();
    if let Some(store) = CALENDAR_STORE.get() {
        // An ICS file is a complete snapshot rather than an incremental feed.
        // Remove vanished events before inserting the newly parsed snapshot.
        store.clear_calendar("local-calendar", "local")?;
        store.apply_sync(
            "local-calendar",
            "local",
            &SyncBatch {
                events: normalized,
                ..Default::default()
            },
        )?;
    }
    for event in events {
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

fn normalize_ics_event(event: &IcsEvent) -> Result<CalendarEvent> {
    Ok(CalendarEvent {
        account_id: "local-calendar".into(),
        provider: CalendarProvider::Local,
        calendar_id: "local".into(),
        event_id: event.id.clone(),
        title: event.title.clone(),
        start_time: normalize_ics_time(&event.start)?,
        end_time: normalize_ics_time(&event.end)?,
        organizer: event.organizer.clone(),
        all_day: event.start.len() == 8,
        cancelled: false,
    })
}

fn normalize_ics_time(value: &str) -> Result<String> {
    let value = value.trim();
    let parsed = if value.len() == 8 {
        chrono::NaiveDate::parse_from_str(value, "%Y%m%d")?
            .and_hms_opt(0, 0, 0)
            .context("invalid all-day ICS timestamp")?
            .and_utc()
    } else if value.ends_with('Z') {
        chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")?.and_utc()
    } else {
        bail!("floating and TZID-based ICS timestamps are not yet supported")
    };
    Ok(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn calendar_state_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("focaldesk/calendar.db")
}

fn dispatch_due_reminders(now: u64) {
    let Some(store) = CALENDAR_STORE.get() else {
        return;
    };
    let reminder_minutes = load_calendar_accounts()
        .map(|document| {
            document
                .accounts
                .into_iter()
                .filter(|account| account.enabled)
                .map(|account| (account.id, account.reminder_minutes))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let Ok(reminders) = store.claim_due_reminders(now as i64, 15 * 60, &reminder_minutes) else {
        return;
    };
    for reminder in reminders {
        let starts = reminder
            .event
            .start_time
            .replace('T', " ")
            .trim_end_matches('Z')
            .to_string();
        let _ = send_notification_request(&NotificationIpcRequest::Notify {
            title: format!("Upcoming: {}", reminder.event.title),
            body: format!("Starts at {starts}"),
            timeout_ms: None,
        });
    }
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
    if matches!(
        connector.manifest.id.as_str(),
        "google-calendar" | "microsoft-calendar"
    ) {
        return 300;
    }
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
    // The socket itself is owner-only (0600), and SO_PEERCRED rejects a
    // different UID.  Do not require /proc/<pid>/exe here: Yama denies that
    // lookup between sibling user services on hardened desktops.
    if let Err(error) = transport::require_same_user(stream) {
        tracing::warn!(%error, "rejected connector-host IPC peer");
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
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: Vec::new(),
            available_calendars: Vec::new(),
        });
    if let Ok(encoded) = transport::encode_message(&response) {
        let _ = stream.write_all(&encoded);
    }
}

fn apply_control_request(
    host_state: &Arc<Mutex<HostState>>,
    request: ConnectorHostIpcRequest,
) -> ConnectorHostIpcResponse {
    if let ConnectorHostIpcRequest::CalendarConnect {
        provider,
        account_id,
        client_id,
        client_secret,
        tenant,
    } = request
    {
        return match start_calendar_oauth(
            host_state,
            provider,
            account_id,
            client_id,
            client_secret,
            tenant,
        ) {
            Ok(flow) => ConnectorHostIpcResponse {
                status: "ok".into(),
                message: None,
                paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
                connectors: Vec::new(),
                calendar_events: Vec::new(),
                oauth: Some(flow),
                calendar_accounts: Vec::new(),
                available_calendars: Vec::new(),
            },
            Err(error) => ConnectorHostIpcResponse {
                status: "error".into(),
                message: Some(error.to_string()),
                paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
                connectors: Vec::new(),
                calendar_events: Vec::new(),
                oauth: None,
                calendar_accounts: Vec::new(),
                available_calendars: Vec::new(),
            },
        };
    }
    if let ConnectorHostIpcRequest::CalendarOAuthStatus { flow_id } = request {
        let (paused, oauth) = host_state
            .lock()
            .map(|s| (s.paused, s.oauth_flows.get(&flow_id).cloned()))
            .unwrap_or((true, None));
        return ConnectorHostIpcResponse {
            status: if oauth.is_some() { "ok" } else { "error" }.into(),
            message: oauth
                .is_none()
                .then(|| "unknown calendar OAuth flow".into()),
            paused,
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth,
            calendar_accounts: Vec::new(),
            available_calendars: Vec::new(),
        };
    }
    if matches!(request, ConnectorHostIpcRequest::CalendarAccounts) {
        let result = calendar_account_snapshots(host_state);
        return ConnectorHostIpcResponse {
            status: if result.is_ok() { "ok" } else { "error" }.into(),
            message: result.as_ref().err().map(ToString::to_string),
            paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: result.unwrap_or_default(),
            available_calendars: Vec::new(),
        };
    }
    if let ConnectorHostIpcRequest::CalendarDiscover { account_id } = request {
        let result = discover_calendars(&account_id);
        return ConnectorHostIpcResponse {
            status: if result.is_ok() { "ok" } else { "error" }.into(),
            message: result.as_ref().err().map(ToString::to_string),
            paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: Vec::new(),
            available_calendars: result.unwrap_or_default(),
        };
    }
    if let ConnectorHostIpcRequest::CalendarUpdatePreferences {
        account_id,
        calendar_ids,
        reminder_minutes,
    } = request
    {
        let result = update_calendar_preferences(&account_id, calendar_ids, reminder_minutes);
        return ConnectorHostIpcResponse {
            status: if result.is_ok() { "ok" } else { "error" }.into(),
            message: result.err().map(|error| error.to_string()),
            paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: Vec::new(),
            available_calendars: Vec::new(),
        };
    }
    if let ConnectorHostIpcRequest::CalendarSyncNow { account_id } = request {
        let connector_id = load_calendar_accounts().and_then(|document| {
            document
                .accounts
                .into_iter()
                .find(|account| account.id == account_id)
                .map(|account| match account.provider {
                    CalendarProvider::Google => "google-calendar",
                    CalendarProvider::Microsoft => "microsoft-calendar",
                    CalendarProvider::Local => "local-calendar",
                })
                .context("unknown calendar account")
        });
        let mut state = match host_state.lock() {
            Ok(state) => state,
            Err(_) => return error_control_response("connector host state unavailable"),
        };
        let mut message = connector_id.as_ref().err().map(ToString::to_string);
        if let Ok(connector_id) = connector_id {
            if let Some(runtime) = state.runtimes.get_mut(connector_id) {
                runtime.force_poll = true;
                if let Some(status) = runtime.status.as_mut() {
                    status.next_run_at_unix = Some(0);
                }
            } else {
                message = Some("calendar connector unavailable".into());
            }
        }
        return ConnectorHostIpcResponse {
            status: if message.is_some() { "error" } else { "ok" }.into(),
            message,
            paused: state.paused,
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: Vec::new(),
            available_calendars: Vec::new(),
        };
    }
    if let ConnectorHostIpcRequest::CalendarDisconnect {
        account_id,
        delete_local_data,
    } = request
    {
        let result = disconnect_calendar_account(&account_id, delete_local_data);
        return ConnectorHostIpcResponse {
            status: if result.is_ok() { "ok" } else { "error" }.into(),
            message: result.err().map(|e| e.to_string()),
            paused: host_state.lock().map(|s| s.paused).unwrap_or(true),
            connectors: Vec::new(),
            calendar_events: Vec::new(),
            oauth: None,
            calendar_accounts: Vec::new(),
            available_calendars: Vec::new(),
        };
    }
    let mut state = match host_state.lock() {
        Ok(state) => state,
        Err(_) => {
            return ConnectorHostIpcResponse {
                status: "error".into(),
                message: Some("connector host state unavailable".into()),
                paused: true,
                connectors: Vec::new(),
                calendar_events: Vec::new(),
                oauth: None,
                calendar_accounts: Vec::new(),
                available_calendars: Vec::new(),
            };
        }
    };
    let mut message = None;
    let mut calendar_events = Vec::new();
    let oauth = None;
    match request {
        ConnectorHostIpcRequest::Status => {}
        ConnectorHostIpcRequest::CalendarUpcoming {
            after_unix,
            before_unix,
            limit,
        } => {
            if before_unix <= after_unix || limit == 0 || limit > 500 {
                message = Some("calendar query bounds are invalid".into());
            } else if let Some(store) = CALENDAR_STORE.get() {
                match store.upcoming(after_unix, before_unix, limit) {
                    Ok(events) => {
                        calendar_events = events
                            .into_iter()
                            .map(|event| CalendarEventSnapshot {
                                account_id: event.account_id,
                                provider: format!("{:?}", event.provider).to_ascii_lowercase(),
                                calendar_id: event.calendar_id,
                                event_id: event.event_id,
                                title: event.title,
                                start_time: event.start_time,
                                end_time: event.end_time,
                                organizer: event.organizer,
                                all_day: event.all_day,
                            })
                            .collect()
                    }
                    Err(error) => message = Some(error.to_string()),
                }
            } else {
                message = Some("calendar store unavailable".into());
            }
        }
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
        ConnectorHostIpcRequest::CalendarConnect { .. }
        | ConnectorHostIpcRequest::CalendarOAuthStatus { .. }
        | ConnectorHostIpcRequest::CalendarAccounts
        | ConnectorHostIpcRequest::CalendarDiscover { .. }
        | ConnectorHostIpcRequest::CalendarUpdatePreferences { .. }
        | ConnectorHostIpcRequest::CalendarSyncNow { .. }
        | ConnectorHostIpcRequest::CalendarDisconnect { .. } => unreachable!(),
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
        calendar_events,
        oauth,
        calendar_accounts: Vec::new(),
        available_calendars: Vec::new(),
    }
}

fn error_control_response(message: &str) -> ConnectorHostIpcResponse {
    ConnectorHostIpcResponse {
        status: "error".into(),
        message: Some(message.into()),
        paused: true,
        connectors: Vec::new(),
        calendar_events: Vec::new(),
        oauth: None,
        calendar_accounts: Vec::new(),
        available_calendars: Vec::new(),
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
