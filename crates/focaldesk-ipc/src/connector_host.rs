use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::transport;

pub const CONNECTOR_HOST_SOCKET_ENV: &str = "FOCALD_CONNECTORS_SOCKET";
pub const CONNECTOR_HOST_SOCKET_NAME: &str = "focald-connectors.sock";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectorHostIpcRequest {
    Status,
    CalendarUpcoming {
        after_unix: i64,
        before_unix: i64,
        #[serde(default = "default_calendar_limit")]
        limit: usize,
    },
    CalendarConnect {
        provider: String,
        account_id: String,
        client_id: String,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        tenant: Option<String>,
    },
    CalendarOAuthStatus {
        flow_id: String,
    },
    CalendarAccounts,
    CalendarDiscover {
        account_id: String,
    },
    CalendarUpdatePreferences {
        account_id: String,
        calendar_ids: Vec<String>,
        reminder_minutes: u32,
    },
    CalendarSyncNow {
        account_id: String,
    },
    CalendarDisconnect {
        account_id: String,
        #[serde(default)]
        delete_local_data: bool,
    },
    PollNow {
        #[serde(default)]
        connector_id: Option<String>,
    },
    SetPaused {
        paused: bool,
    },
    ClearQuarantine {
        connector_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarOAuthSnapshot {
    pub flow_id: String,
    pub provider: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

const fn default_calendar_limit() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarEventSnapshot {
    pub account_id: String,
    pub provider: String,
    pub calendar_id: String,
    pub event_id: String,
    pub title: String,
    pub start_time: String,
    pub end_time: String,
    pub organizer: String,
    pub all_day: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarAccountSnapshot {
    pub account_id: String,
    pub provider: String,
    pub connected: bool,
    pub enabled: bool,
    pub cached_events: u64,
    pub sync_state: String,
    #[serde(default)]
    pub calendar_ids: Vec<String>,
    #[serde(default = "default_reminder_minutes")]
    pub reminder_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

const fn default_reminder_minutes() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarAvailableSnapshot {
    pub id: String,
    pub name: String,
    pub primary: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorRuntimeStatus {
    pub connector_id: String,
    pub state: String,
    pub consecutive_failures: u32,
    pub quarantined: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub delivered_events: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorHostIpcResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub paused: bool,
    #[serde(default)]
    pub connectors: Vec<ConnectorRuntimeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendar_events: Vec<CalendarEventSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<CalendarOAuthSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendar_accounts: Vec<CalendarAccountSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_calendars: Vec<CalendarAvailableSnapshot>,
}

pub fn connector_host_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(CONNECTOR_HOST_SOCKET_ENV, CONNECTOR_HOST_SOCKET_NAME)
}

pub fn send_connector_host_request(
    request: &ConnectorHostIpcRequest,
) -> Result<ConnectorHostIpcResponse, String> {
    let path = connector_host_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("could not connect to {}: {error}", path.display()))?;
    transport::configure_stream(&stream).map_err(|error| error.to_string())?;
    let encoded = transport::encode_message(request)?;
    stream
        .write_all(&encoded)
        .map_err(|error| error.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    transport::decode_message(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_requests_round_trip() {
        for request in [
            ConnectorHostIpcRequest::Status,
            ConnectorHostIpcRequest::CalendarUpcoming {
                after_unix: 0,
                before_unix: 86_400,
                limit: 10,
            },
            ConnectorHostIpcRequest::CalendarConnect {
                provider: "google".into(),
                account_id: "school".into(),
                client_id: "client".into(),
                client_secret: None,
                tenant: None,
            },
            ConnectorHostIpcRequest::CalendarOAuthStatus {
                flow_id: "flow".into(),
            },
            ConnectorHostIpcRequest::CalendarAccounts,
            ConnectorHostIpcRequest::CalendarDiscover {
                account_id: "school".into(),
            },
            ConnectorHostIpcRequest::CalendarUpdatePreferences {
                account_id: "school".into(),
                calendar_ids: vec!["primary".into()],
                reminder_minutes: 15,
            },
            ConnectorHostIpcRequest::CalendarSyncNow {
                account_id: "school".into(),
            },
            ConnectorHostIpcRequest::CalendarDisconnect {
                account_id: "school".into(),
                delete_local_data: true,
            },
            ConnectorHostIpcRequest::PollNow {
                connector_id: Some("service-health".into()),
            },
            ConnectorHostIpcRequest::SetPaused { paused: true },
            ConnectorHostIpcRequest::ClearQuarantine {
                connector_id: "service-health".into(),
            },
        ] {
            let encoded = serde_json::to_vec(&request).unwrap();
            let decoded = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(request, decoded);
        }
    }
}
