use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::transport;

pub const SPEECH_SOCKET_ENV: &str = "FOCALD_SPEECH_SOCKET";
pub const SPEECH_SOCKET_NAME: &str = "focald-speech.sock";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeechPriority {
    #[default]
    Normal,
    Interrupt,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeechCommand {
    Speak,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeechIpcRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<SpeechCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub priority: SpeechPriority,
    #[serde(default)]
    pub replace: bool,
}

impl SpeechIpcRequest {
    pub fn interrupt(text: impl Into<String>) -> Self {
        Self {
            command: Some(SpeechCommand::Speak),
            text: Some(text.into()),
            priority: SpeechPriority::Interrupt,
            replace: true,
        }
    }

    pub fn stop() -> Self {
        Self {
            command: Some(SpeechCommand::Stop),
            text: None,
            priority: SpeechPriority::Normal,
            replace: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeechIpcResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn speech_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(SPEECH_SOCKET_ENV, SPEECH_SOCKET_NAME)
}

pub fn send_speech_request(request: &SpeechIpcRequest) -> Result<SpeechIpcResponse, String> {
    let path = speech_socket_path()?;
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
    fn interrupt_request_matches_the_daemon_contract() {
        let value = serde_json::to_value(SpeechIpcRequest::interrupt("hello")).unwrap();
        assert_eq!(value["command"], "speak");
        assert_eq!(value["priority"], "interrupt");
        assert_eq!(value["replace"], true);
    }
}
