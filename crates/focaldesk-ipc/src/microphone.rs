use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::transport;

pub const MICROPHONE_SOCKET_ENV: &str = "FOCALD_MIC_SOCKET";
pub const MICROPHONE_SOCKET_NAME: &str = "focald-mic.sock";

fn legacy_requester() -> String {
    "focaldesk-desktop".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum MicrophoneIpcRequest {
    Start {
        #[serde(default = "legacy_requester")]
        requester: String,
    },
    StartAmbient {
        requester: String,
        wake_phrase: String,
        #[serde(default)]
        blocked_applications: Vec<String>,
    },
    Stop {
        #[serde(default)]
        lease_id: Option<String>,
    },
    Toggle {
        #[serde(default = "legacy_requester")]
        requester: String,
    },
    Status,
    Poll {
        lease_id: String,
        #[serde(default)]
        after_sequence: u64,
    },
    ClearEvents {
        lease_id: String,
    },
    Kill,
    Enable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MicrophoneEvent {
    Ready,
    Partial(String),
    Final(String),
    VoiceActivity(bool),
    WakeDetected,
    Command(String),
    Stopped,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicrophoneEventRecord {
    pub sequence: u64,
    pub event: MicrophoneEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicrophoneIpcResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub killed: bool,
    #[serde(default)]
    pub latest_sequence: u64,
    #[serde(default)]
    pub events: Vec<MicrophoneEventRecord>,
}

pub fn microphone_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(MICROPHONE_SOCKET_ENV, MICROPHONE_SOCKET_NAME)
}

pub fn send_microphone_request(
    request: &MicrophoneIpcRequest,
) -> Result<MicrophoneIpcResponse, String> {
    let path = microphone_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("could not connect to {}: {error}", path.display()))?;
    transport::configure_stream(&stream).map_err(|error| error.to_string())?;
    let payload = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let encoded = transport::encode_message(&payload)?;
    stream
        .write_all(&encoded)
        .map_err(|error| error.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| error.to_string())?;
    let payload: String = transport::decode_message(response.as_bytes())?;
    serde_json::from_str(&payload).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_start_defaults_to_the_desktop_requester() {
        let request: MicrophoneIpcRequest = serde_json::from_str(r#"{"command":"start"}"#).unwrap();
        assert_eq!(
            request,
            MicrophoneIpcRequest::Start {
                requester: "focaldesk-desktop".into()
            }
        );
    }

    #[test]
    fn ambient_request_round_trips() {
        let request = MicrophoneIpcRequest::StartAmbient {
            requester: "focaldesk-ai-console".into(),
            wake_phrase: "hello focaldesk".into(),
            blocked_applications: vec!["example".into()],
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded = serde_json::from_str(&encoded).unwrap();
        assert_eq!(request, decoded);
    }
}
