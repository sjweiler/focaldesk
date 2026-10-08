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
