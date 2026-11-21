//! Calendar Settings client. OAuth URLs and tokens never enter the webview.
use focaldesk_ipc::{
    send_connector_host_request, CalendarAccountSnapshot, CalendarAvailableSnapshot,
    CalendarEventSnapshot, CalendarOAuthSnapshot, ConnectorHostIpcRequest as Request,
    ConnectorHostIpcResponse as Response,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use url::Url;

#[derive(Serialize, Clone)]
pub struct PublicFlow {
    pub flow_id: String,
    pub account_id: String,
    pub provider: String,
    pub state: String,
    pub user_code: Option<String>,
    pub browser_available: bool,
    pub message: String,
}
struct Flow {
    public: PublicFlow,
    url: Option<Url>,
    expires: Instant,
}
#[derive(Default)]
pub struct CalendarService {
    operation: Mutex<()>,
    flow: Mutex<Option<Flow>>,
}
#[derive(Serialize)]
pub struct CalendarSnapshot {
    pub accounts: Vec<CalendarAccountSnapshot>,
    pub events: Vec<CalendarEventSnapshot>,
    pub flow: Option<PublicFlow>,
    pub paused: bool,
}
#[derive(Serialize)]
pub struct CalendarPreferences {
    pub account: CalendarAccountSnapshot,
    pub calendars: Vec<CalendarAvailableSnapshot>,
}
#[derive(Serialize)]
pub struct CalendarResult {
    pub message: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CalendarAction {
    Sync {
        account_id: String,
    },
    Disconnect {
        account_id: String,
        delete_local_data: bool,
    },
    Preferences {
        account_id: String,
        expected_calendar_ids: Vec<String>,
        expected_reminder_minutes: u32,
        calendar_ids: Vec<String>,
        reminder_minutes: u32,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectCalendar {
    provider: String,
    account_id: String,
    client_id: String,
    client_secret: Option<String>,
    tenant: Option<String>,
}

fn send(request: &Request) -> Result<Response, String> {
    let response = send_connector_host_request(request)?;
    if response.status != "ok" {
        if response.message.as_ref().is_some_and(|message| {
            message.contains("unknown variant")
                && message.contains("calendar_update_preferences_checked")
        }) {
            return Err("The running Calendar service needs an update. Rebuild and restart focald-connectors before saving preferences.".into());
        }
        return Err(response
            .message
            .unwrap_or_else(|| "Calendar service rejected the operation.".into()));
    }
    Ok(response)
}
fn account_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn validate_connect(input: &ConnectCalendar) -> Result<(), String> {
    if !matches!(input.provider.as_str(), "google" | "microsoft")
        || !account_id(&input.account_id)
        || input.client_id.trim().is_empty()
        || input.client_id.len() > 256
        || input.client_id.chars().any(char::is_control)
    {
        return Err("Choose a provider, an account name using letters/numbers/dashes/underscores, and an OAuth client ID.".into());
    }
    if input
        .client_secret
        .as_ref()
        .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err("Invalid OAuth application secret.".into());
    }
    if input.provider == "microsoft" {
        let tenant = input.tenant.as_deref().unwrap_or("common");
        if tenant.is_empty()
            || tenant.len() > 128
            || !tenant
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        {
            return Err("Enter a valid Microsoft tenant or common.".into());
        }
        if input
            .client_secret
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        {
            return Err("Microsoft browser sign-in uses PKCE without a client secret.".into());
        }
    }
    Ok(())
}
fn validate_preferences(ids: &[String], minutes: u32) -> Result<(), String> {
    if ids.is_empty()
        || ids.len() > 32
        || ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || !matches!(minutes, 0 | 5 | 10 | 15 | 30 | 60)
    {
        return Err("Select 1–32 distinct calendars and a supported reminder time.".into());
    }
    Ok(())
}
fn browser_url(provider: &str, value: &str) -> Result<Url, String> {
    let url =
        Url::parse(value).map_err(|_| "The calendar service returned an invalid sign-in link.")?;
    let allowed = match provider {
        "google" => {
            url.host_str() == Some("accounts.google.com") && url.path() == "/o/oauth2/v2/auth"
        }
        "microsoft" => {
            url.host_str() == Some("login.microsoftonline.com")
                && url.path().ends_with("/oauth2/v2.0/authorize")
        }
        _ => false,
    };
    if !allowed
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
        || url.query_pairs().any(|(key, _)| {
            matches!(
                key.as_ref(),
                "access_token" | "refresh_token" | "client_secret" | "code_verifier" | "id_token"
            )
        })
    {
        return Err("The calendar service returned an unsupported sign-in link.".into());
    }
    Ok(url)
}
fn public_flow(flow: CalendarOAuthSnapshot, account_id: String, url: bool) -> PublicFlow {
    let (state, message) = match flow.state.as_str() {
        "connected" => (
            "connected",
            "Connected. Calendar sync may require consent in AI Console.",
        ),
        "error" => (
            "error",
            "Sign-in failed or expired. Check your application configuration and try again.",
        ),
        _ => (
            "waiting_for_browser",
            "Finish signing in with your browser. Sign-in can take up to 10 minutes.",
        ),
    };
    PublicFlow {
        flow_id: flow.flow_id,
        account_id,
        provider: flow.provider,
        state: state.into(),
        user_code: flow.user_code,
        browser_available: url && state == "waiting_for_browser",
        message: message.into(),
    }
}
fn resolve(account_id: &str) -> Result<CalendarAccountSnapshot, String> {
    if !self::account_id(account_id) {
        return Err("Invalid calendar account name.".into());
    }
    send(&Request::CalendarAccounts)?
        .calendar_accounts
        .into_iter()
        .find(|account| account.account_id == account_id)
        .map(safe_account)
        .ok_or_else(|| "This account no longer exists. Refresh accounts before continuing.".into())
}
fn safe_account(mut account: CalendarAccountSnapshot) -> CalendarAccountSnapshot {
    if account.last_error.is_some() {
        account.last_error =
            Some("Sync needs attention. Check sign-in, permissions, and calendar consent.".into());
    }
    account
}
impl CalendarService {
    pub fn snapshot(&self) -> Result<CalendarSnapshot, String> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| "Calendar controls unavailable.")?;
        let accounts = send(&Request::CalendarAccounts)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock unavailable.")?
            .as_secs() as i64;
        let events = send(&Request::CalendarUpcoming {
            after_unix: now,
            before_unix: now + 7 * 86_400,
            limit: 20,
        })?;
        let account_list = accounts
            .calendar_accounts
            .into_iter()
            .map(safe_account)
            .collect();
        let flow = self
            .flow
            .lock()
            .map_err(|_| "Calendar sign-in unavailable.")?
            .as_ref()
            .map(|flow| flow.public.clone());
        Ok(CalendarSnapshot {
            accounts: account_list,
            events: events.calendar_events,
            flow,
            paused: accounts.paused,
        })
    }
    pub fn discover(&self, id: String) -> Result<CalendarPreferences, String> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| "Calendar controls unavailable.")?;
        let account = resolve(&id)?;
        if !account.connected {
            return Err("Connect this account before discovering calendars.".into());
        }
        let mut calendars = send(&Request::CalendarDiscover {
            account_id: id.clone(),
        })?
        .available_calendars;
        let account = resolve(&id)?;
        for calendar in &mut calendars {
            calendar.selected = account.calendar_ids.contains(&calendar.id);
        }
        for id in &account.calendar_ids {
            if !calendars.iter().any(|calendar| calendar.id == *id) {
                calendars.push(CalendarAvailableSnapshot {
                    id: id.clone(),
                    name: format!("{id} (saved calendar)"),
                    primary: false,
                    selected: true,
                });
            }
        }
        Ok(CalendarPreferences { account, calendars })
    }
    pub fn apply(&self, action: CalendarAction) -> Result<CalendarResult, String> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| "Calendar controls unavailable.")?;
        let (request, message) = match action {
            CalendarAction::Sync { account_id } => {
                let account = resolve(&account_id)?;
                if !account.connected || !account.enabled {
                    return Err("Reconnect or enable this account before queuing a sync.".into());
                }
                (
                    Request::CalendarSyncNow { account_id },
                    "Sync queued. Refresh to check its progress.",
                )
            }
            CalendarAction::Disconnect {
                account_id,
                delete_local_data,
            } => {
                resolve(&account_id)?;
                (
                    Request::CalendarDisconnect {
                        account_id,
                        delete_local_data,
                    },
                    if delete_local_data {
                        "Account disconnected; credentials and local calendar data removed."
                    } else {
                        "Account disconnected; credentials removed and cached events retained."
                    },
                )
            }
            CalendarAction::Preferences {
                account_id,
                expected_calendar_ids,
                expected_reminder_minutes,
                calendar_ids,
                reminder_minutes,
            } => {
                validate_preferences(&calendar_ids, reminder_minutes)?;
                let account = resolve(&account_id)?;
                if account.calendar_ids.iter().collect::<BTreeSet<_>>()
                    != expected_calendar_ids.iter().collect::<BTreeSet<_>>()
                    || account.reminder_minutes != expected_reminder_minutes
                {
                    return Err("Calendar preferences changed in another client. Reload calendars before applying.".into());
                }
                (
                    Request::CalendarUpdatePreferencesChecked {
                        account_id,
                        expected_calendar_ids,
                        expected_reminder_minutes,
                        calendar_ids,
                        reminder_minutes,
                    },
                    "Calendar selection and reminders saved.",
                )
            }
        };
        send(&request)?;
        Ok(CalendarResult {
            message: message.into(),
        })
    }
    pub fn connect(&self, input: ConnectCalendar) -> Result<PublicFlow, String> {
        validate_connect(&input)?;
        let _guard = self
            .operation
            .lock()
            .map_err(|_| "Calendar controls unavailable.")?;
        let mut stored = self
            .flow
            .lock()
            .map_err(|_| "Calendar sign-in unavailable.")?;
        if let Some(flow) = stored.as_mut() {
            if flow.public.state == "waiting_for_browser" && Instant::now() < flow.expires {
                if let Some(status) = send(&Request::CalendarOAuthStatus {
                    flow_id: flow.public.flow_id.clone(),
                })?
                .oauth
                {
                    flow.public =
                        public_flow(status, flow.public.account_id.clone(), flow.url.is_some());
                }
                if flow.public.state == "waiting_for_browser" {
                    return Err("A sign-in is already in progress. Finish it or wait for it to expire before starting another.".into());
                }
            }
        }
        if let Some(existing) = send(&Request::CalendarAccounts)?
            .calendar_accounts
            .iter()
            .find(|account| account.account_id == input.account_id)
        {
            if existing.connected || existing.provider != input.provider {
                return Err("This account name is already connected or belongs to another provider. Choose another account name.".into());
            }
        }
        let id = input.account_id.clone();
        let provider = input.provider.clone();
        let request = Request::CalendarConnect {
            provider: input.provider,
            account_id: input.account_id,
            client_id: input.client_id,
            client_secret: input.client_secret,
            tenant: input.tenant,
        };
        let response=send(&request).map_err(|_|"Could not start calendar sign-in. Check the connector service and OAuth application configuration.".to_string())?;
        let snapshot = response
            .oauth
            .ok_or("The calendar service did not return a sign-in flow.")?;
        if snapshot.provider != provider {
            return Err("The calendar service returned the wrong sign-in provider.".into());
        }
        let url = snapshot
            .verification_uri
            .as_deref()
            .map(|value| browser_url(&provider, value))
            .transpose()?;
        let public = public_flow(snapshot, id, url.is_some());
        *stored = Some(Flow {
            public: public.clone(),
            url,
            expires: Instant::now() + Duration::from_secs(10 * 60),
        });
        Ok(public)
    }
    pub fn poll(&self, id: String) -> Result<PublicFlow, String> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| "Calendar controls unavailable.")?;
        let mut stored = self
            .flow
            .lock()
            .map_err(|_| "Calendar sign-in unavailable.")?;
        let flow = stored
            .as_mut()
            .filter(|flow| flow.public.flow_id == id)
            .ok_or("This sign-in is no longer available. Start a new sign-in.")?;
        if Instant::now() >= flow.expires {
            flow.public.state = "error".into();
            flow.public.message = "Sign-in expired. Start a new sign-in.".into();
            flow.public.browser_available = false;
            flow.url = None;
            return Ok(flow.public.clone());
        }
        let status = send(&Request::CalendarOAuthStatus { flow_id: id })?
            .oauth
            .ok_or("Calendar sign-in status unavailable.")?;
        flow.public = public_flow(status, flow.public.account_id.clone(), flow.url.is_some());
        if flow.public.state != "waiting_for_browser" {
            flow.url = None;
        }
        Ok(flow.public.clone())
    }
    pub fn open_browser(&self, id: String) -> Result<(), String> {
        let stored = self
            .flow
            .lock()
            .map_err(|_| "Calendar sign-in unavailable.")?;
        let flow = stored
            .as_ref()
            .filter(|flow| {
                flow.public.flow_id == id
                    && flow.public.state == "waiting_for_browser"
                    && Instant::now() < flow.expires
            })
            .ok_or("This sign-in is no longer waiting for a browser.")?;
        let url = flow
            .url
            .as_ref()
            .ok_or("No browser link is available for this sign-in.")?;
        let mut child = Command::new("xdg-open")
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                "Could not launch the browser. Check your default browser and try again."
            })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("The browser launcher failed. Check your default browser and try again.".into())
                    }
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(
                        "The browser launcher timed out. You can retry opening sign-in.".into(),
                    );
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_preferences_and_sign_in_links() {
        assert!(validate_preferences(&["primary".into()], 10).is_ok());
        assert!(validate_preferences(&[], 10).is_err());
        assert!(validate_preferences(&["a".into(), "a".into()], 10).is_err());
        assert!(validate_preferences(&["a".into()], 7).is_err());
        assert!(browser_url(
            "google",
            "https://accounts.google.com/o/oauth2/v2/auth?state=public-state&code_challenge=pkce"
        )
        .is_ok());
        for link in [
            "file:///tmp/script",
            "https://accounts.google.com.evil.test/o/oauth2/v2/auth",
            "https://accounts.google.com/o/oauth2/v2/auth?access_token=private",
            "https://user@accounts.google.com/o/oauth2/v2/auth",
        ] {
            assert!(browser_url("google", link).is_err());
        }
    }
    #[test]
    fn public_oauth_projection_does_not_include_the_sign_in_url_or_provider_errors() {
        let flow = CalendarOAuthSnapshot {
            flow_id: "flow".into(),
            provider: "google".into(),
            state: "error".into(),
            verification_uri: Some("private-url".into()),
            user_code: None,
            message: Some("access_token=private-token".into()),
        };
        let serialized = serde_json::to_string(&public_flow(flow, "work".into(), true)).unwrap();
        assert!(!serialized.contains("private-url"));
        assert!(!serialized.contains("private-token"));
    }
}
