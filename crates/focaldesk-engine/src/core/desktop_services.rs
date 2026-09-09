//! Slow desktop-service polling kept outside the compositor state machine.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use focaldesk_ipc::{send_update_request, transport, UpdateIpcRequest, UpdateIpcResponse};
use focaldesk_logging::{flog_info, flog_warn};
use focaldesk_network::model::NetworkState;
use focaldesk_updates::UpdateSnapshot;

use crate::core::ui_builder::VoiceCaptureStatus;

pub(super) fn mic_command(command: &str) -> io::Result<String> {
    let socket =
        transport::socket_path("FOCALD_MIC_SOCKET", "focald-mic.sock").map_err(io::Error::other)?;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request = format!(r#"{{"command":"{command}"}}"#);
    let request = transport::encode_message(&request).map_err(io::Error::other)?;
    stream.write_all(&request)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    transport::decode_message(response.as_bytes()).map_err(io::Error::other)
}

pub(super) fn voice_capture_status(response: &str) -> Option<VoiceCaptureStatus> {
    let status = serde_json::from_str::<serde_json::Value>(response)
        .ok()?
        .get("status")?
        .as_str()?
        .to_owned();
    match status.as_str() {
        "idle" => Some(VoiceCaptureStatus::Idle),
        "starting" => Some(VoiceCaptureStatus::Starting),
        "listening" => Some(VoiceCaptureStatus::Listening),
        "stopping" => Some(VoiceCaptureStatus::Stopping),
        _ => None,
    }
}

pub(super) fn toggle_voice_capture(status_tx: mpsc::Sender<VoiceCaptureStatus>) {
    let _ = thread::Builder::new()
        .name("focaldesk-voice-toggle".into())
        .spawn(move || match mic_command("toggle") {
            Ok(response) => {
                flog_info!("voice capture: {}", response.trim());
                let status =
                    voice_capture_status(&response).unwrap_or(VoiceCaptureStatus::Unavailable);
                let _ = status_tx.send(status);
            }
            Err(err) => {
                flog_warn!("voice capture toggle failed: {err}");
                let _ = status_tx.send(VoiceCaptureStatus::Unavailable);
            }
        });
}

/// Runs `focaldesk-network`'s async backend to completion on a throwaway
/// current-thread tokio runtime. Called from a one-shot background thread
/// (see `process_network_state_timers`), matching the compositor's existing
/// poll-and-spawn idiom for out-of-process state (mic detection, voice
/// capture status) rather than keeping a persistent async runtime/task
/// alive inside the otherwise-synchronous compositor.
pub(super) fn poll_network_state() -> NetworkState {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return NetworkState::default();
    };

    runtime.block_on(async {
        match focaldesk_network::auto_backend().await {
            Ok(backend) => backend.current_state().await.unwrap_or_default(),
            Err(_) => NetworkState::default(),
        }
    })
}

pub(super) fn poll_update_state() -> UpdateSnapshot {
    match send_update_request(&UpdateIpcRequest::GetState) {
        Ok(UpdateIpcResponse::State { snapshot }) => snapshot,
        Ok(UpdateIpcResponse::Error { message }) => {
            flog_warn!("update state request rejected: {message}");
            UpdateSnapshot::default()
        }
        Ok(other) => {
            flog_warn!("unexpected update state response: {other:?}");
            UpdateSnapshot::default()
        }
        Err(_) => UpdateSnapshot::default(),
    }
}
