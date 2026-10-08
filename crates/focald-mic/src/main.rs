//! Exclusive microphone-session and speech-to-text daemon for FocalDesk.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use focaldesk_ipc::{
    transport, MicrophoneEvent, MicrophoneEventRecord, MicrophoneIpcRequest, MicrophoneIpcResponse,
};
use focaldesk_voice::{AmbientVoiceConfig, VoiceEvent, VoiceSession};

const MAX_EVENTS: usize = 128;
const AMBIENT_LEASE_IDLE: Duration = Duration::from_secs(15);

struct MicLease {
    id: String,
    requester: String,
    ambient: bool,
    last_heartbeat: Instant,
}

#[derive(Default)]
struct MicState {
    session: Option<VoiceSession>,
    events: Option<Receiver<VoiceEvent>>,
    transcript_parts: Vec<String>,
    ready: bool,
    stopping: bool,
    killed: bool,
    lease: Option<MicLease>,
    events_log: VecDeque<MicrophoneEventRecord>,
    next_sequence: u64,
}

impl MicState {
    fn status(&self) -> &'static str {
        if self.stopping {
            "stopping"
        } else if self.session.is_some() && !self.ready {
            "starting"
        } else if self.session.is_some() {
            "listening"
        } else {
            "idle"
        }
    }

    fn response(
        &self,
        message: Option<String>,
        events: Vec<MicrophoneEventRecord>,
    ) -> MicrophoneIpcResponse {
        MicrophoneIpcResponse {
            status: self.status().into(),
            message,
            lease_id: self.lease.as_ref().map(|lease| lease.id.clone()),
            owner: self.lease.as_ref().map(|lease| lease.requester.clone()),
            killed: self.killed,
            latest_sequence: self.next_sequence.saturating_sub(1),
            events,
        }
    }

    fn start(
        &mut self,
        requester: String,
        ambient: Option<AmbientVoiceConfig>,
    ) -> Result<MicrophoneIpcResponse, String> {
        if self.killed {
            return Err("microphone kill switch is active".into());
        }
        if self.session.is_some() {
            let owner = self
                .lease
                .as_ref()
                .map(|lease| lease.requester.as_str())
                .unwrap_or("unknown");
            return Err(format!("microphone is already leased to {owner}"));
        }

        let model_dir =
            focaldesk_voice::find_model_dir().ok_or_else(focaldesk_voice::install_instructions)?;
        stop_speech();
        let (events_tx, events_rx) = mpsc::channel();
        let is_ambient = ambient.is_some();
        let session = match ambient {
            Some(config) => VoiceSession::start_ambient(model_dir, events_tx, config),
            None => VoiceSession::start(model_dir, events_tx),
        }
        .map_err(|err| format!("start microphone capture: {err:#}"))?;
        let lease_id = format!("mic-{:032x}", rand::random::<u128>());
        self.session = Some(session);
        self.events = Some(events_rx);
        self.lease = Some(MicLease {
            id: lease_id,
            requester,
            ambient: is_ambient,
            last_heartbeat: Instant::now(),
        });
        self.events_log.clear();
        self.next_sequence = 1;
        self.transcript_parts.clear();
        self.ready = false;
        self.stopping = false;
        eprintln!("[starting]");
        Ok(self.response(None, Vec::new()))
    }

    fn stop(&mut self, lease_id: Option<&str>) -> Result<MicrophoneIpcResponse, String> {
        if let (Some(supplied), Some(lease)) = (lease_id, self.lease.as_ref()) {
            if supplied != lease.id {
                return Err("microphone lease does not match the active owner".into());
            }
        }
        if let Some(session) = &self.session {
            session.stop();
            self.stopping = true;
            eprintln!("[stopping]");
        }
        Ok(self.response(None, Vec::new()))
    }

    fn toggle(&mut self, requester: String) -> Result<MicrophoneIpcResponse, String> {
        if self.session.is_some() {
            if self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.requester != requester)
            {
                return Err("microphone is leased to another application".into());
            }
            self.stop(None)
        } else {
            self.start(requester, None)
        }
    }

    fn record(&mut self, event: MicrophoneEvent) {
        let record = MicrophoneEventRecord {
            sequence: self.next_sequence,
            event,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.events_log.len() == MAX_EVENTS {
            self.events_log.pop_front();
        }
        self.events_log.push_back(record);
    }

    fn poll(
        &mut self,
        lease_id: &str,
        after_sequence: u64,
    ) -> Result<MicrophoneIpcResponse, String> {
        let lease = self
            .lease
            .as_mut()
            .ok_or_else(|| "microphone lease is no longer active".to_string())?;
        if lease.id != lease_id {
            return Err("microphone lease does not match the active owner".into());
        }
        lease.last_heartbeat = Instant::now();
        let events = self
            .events_log
            .iter()
            .filter(|record| record.sequence > after_sequence)
            .cloned()
            .collect();
        Ok(self.response(None, events))
    }

    fn expire_stale_lease(&mut self) {
        let expired = self.lease.as_ref().is_some_and(|lease| {
            lease.ambient && lease.last_heartbeat.elapsed() > AMBIENT_LEASE_IDLE
        });
        if expired && !self.stopping {
            eprintln!("[lease-expired]");
            let _ = self.stop(None);
        }
    }

    fn poll_events(&mut self) {
        loop {
            let event = match self.events.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Empty)) | None => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    let transcript = std::mem::take(&mut self.transcript_parts).join(" ");
                    let should_forward = self.stopping
                        && self.lease.as_ref().is_some_and(|lease| !lease.ambient)
                        && !transcript.trim().is_empty();
                    self.events = None;
                    self.session = None;
                    self.ready = false;
                    self.stopping = false;
                    eprintln!("[idle]");
                    if should_forward {
                        eprintln!("[transcript] {transcript:?}");
                        forward_transcript(transcript);
                    }
                    break;
                }
            };

            match event {
                VoiceEvent::Ready => {
                    self.ready = true;
                    self.record(MicrophoneEvent::Ready);
                    eprintln!("[listening]");
                }
                VoiceEvent::Partial(text) => self.record(MicrophoneEvent::Partial(text)),
                VoiceEvent::Final(text) if !text.trim().is_empty() => {
                    let text = text.trim().to_string();
                    eprintln!("[transcript-part] {text:?}");
                    if self.lease.as_ref().is_some_and(|lease| !lease.ambient) {
                        self.transcript_parts.push(text.clone());
                    }
                    self.record(MicrophoneEvent::Final(text));
                }
                VoiceEvent::Final(_) => {}
                VoiceEvent::VoiceActivity(active) => {
                    if active {
                        stop_speech();
                    }
                    self.record(MicrophoneEvent::VoiceActivity(active));
                }
                VoiceEvent::WakeDetected => self.record(MicrophoneEvent::WakeDetected),
                VoiceEvent::Command(command) => self.record(MicrophoneEvent::Command(command)),
                VoiceEvent::Stopped => self.record(MicrophoneEvent::Stopped),
                VoiceEvent::Error(message) => {
                    self.record(MicrophoneEvent::Error(message.clone()));
                    eprintln!("[failed] {message}");
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return run_client(&args);
    }
    run_server()
}

fn run_server() -> Result<()> {
    let socket = mic_socket_path()?;
    let listener = transport::bind_user_socket(&socket)
        .with_context(|| format!("bind microphone socket {}", socket.display()))?;
    listener.set_nonblocking(true)?;
    eprintln!("focald-mic: listening at {}", socket.display());

    let mut state = MicState::default();
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Ok(identity) =
                        transport::require_authorized_peer(&stream, transport::MIC_POLICY)
                    {
                        let requester = identity
                            .executable
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("authorized-client")
                            .to_string();
                        handle_client(stream, &mut state, &requester);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => eprintln!("focald-mic: accept failed: {err}"),
            }
        }
        state.poll_events();
        state.expire_stale_lease();
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn handle_client(mut stream: UnixStream, state: &mut MicState, peer_requester: &str) {
    let result = transport::read_limited(&mut stream)
        .map_err(|err| format!("reading request: {err}"))
        .and_then(|payload| transport::decode_message::<String>(&payload))
        .and_then(|payload| decode_request(&payload))
        .and_then(|request| execute_request(request, state, peer_requester));

    let response = match result {
        Ok(response) => response,
        Err(message) => MicrophoneIpcResponse {
            status: "error".into(),
            message: Some(message),
            lease_id: state.lease.as_ref().map(|lease| lease.id.clone()),
            owner: state.lease.as_ref().map(|lease| lease.requester.clone()),
            killed: state.killed,
            latest_sequence: state.next_sequence.saturating_sub(1),
            events: Vec::new(),
        },
    };
    if let Ok(response) = serde_json::to_string(&response) {
        if let Ok(encoded) = transport::encode_message(&response) {
            let _ = stream.write_all(&encoded);
        }
    }
}

fn decode_request(payload: &str) -> Result<MicrophoneIpcRequest, String> {
    serde_json::from_str(payload).map_err(|err| format!("invalid JSON request: {err}"))
}

fn execute_request(
    request: MicrophoneIpcRequest,
    state: &mut MicState,
    peer_requester: &str,
) -> Result<MicrophoneIpcResponse, String> {
    match request {
        MicrophoneIpcRequest::Start { .. } => state.start(peer_requester.to_string(), None),
        MicrophoneIpcRequest::StartAmbient {
            wake_phrase,
            blocked_applications,
            ..
        } => state.start(
            peer_requester.to_string(),
            Some(AmbientVoiceConfig {
                wake_phrase,
                requester_application: peer_requester.to_string(),
                blocked_applications,
                ..AmbientVoiceConfig::default()
            }),
        ),
        MicrophoneIpcRequest::Stop { lease_id } => {
            if lease_id.is_none()
                && state
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.requester != peer_requester)
            {
                return Err("microphone is leased to another application".into());
            }
            state.stop(lease_id.as_deref())
        }
        MicrophoneIpcRequest::Toggle { .. } => state.toggle(peer_requester.to_string()),
        MicrophoneIpcRequest::Status => Ok(state.response(None, Vec::new())),
        MicrophoneIpcRequest::Poll {
            lease_id,
            after_sequence,
        } => state.poll(&lease_id, after_sequence),
        MicrophoneIpcRequest::ClearEvents { lease_id } => {
            if state
                .lease
                .as_ref()
                .is_none_or(|lease| lease.id != lease_id)
            {
                return Err("microphone lease does not match the active owner".into());
            }
            state.events_log.clear();
            Ok(state.response(None, Vec::new()))
        }
        MicrophoneIpcRequest::Kill => {
            focaldesk_voice::set_microphone_killed(true);
            state.killed = true;
            let _ = state.stop(None);
            Ok(state.response(None, Vec::new()))
        }
        MicrophoneIpcRequest::Enable => {
            focaldesk_voice::set_microphone_killed(false);
            state.killed = false;
            Ok(state.response(None, Vec::new()))
        }
    }
}

fn stop_speech() {
    let request = serde_json::json!({ "command": "stop" }).to_string();
    let result = speech_socket_path()
        .and_then(|path| send_socket_request(path, &request, Duration::from_secs(2)));
    if let Err(err) = result {
        eprintln!("focald-mic: could not stop speech playback: {err:#}");
    }
}

fn forward_transcript(text: String) {
    let _ = std::thread::Builder::new()
        .name("focald-mic-forward".into())
        .spawn(move || {
            let result = voice_socket_path()
                .and_then(|path| send_socket_request(path, &text, Duration::from_secs(20)));
            match result {
                Ok(response) => eprintln!("[forwarded] {}", response.trim()),
                Err(err) => eprintln!("[forward-failed] {err:#}"),
            }
        });
}

fn send_socket_request(path: PathBuf, payload: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(1));
    let mut stream = loop {
        match UnixStream::connect(&path) {
            Ok(stream) => break stream,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                return Err(err).with_context(|| format!("connect to {}", path.display()));
            }
        }
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let payload = transport::encode_message(&payload.to_string()).map_err(anyhow::Error::msg)?;
    stream.write_all(&payload)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    transport::decode_message(response.as_bytes()).map_err(anyhow::Error::msg)
}

fn run_client(args: &[String]) -> Result<()> {
    let request = match args {
        [arg] if arg == "--start" => MicrophoneIpcRequest::Start {
            requester: "focald-mic-cli".into(),
        },
        [arg] if arg == "--stop" => MicrophoneIpcRequest::Stop { lease_id: None },
        [arg] if arg == "--toggle" => MicrophoneIpcRequest::Toggle {
            requester: "focald-mic-cli".into(),
        },
        [arg] if arg == "--status" => MicrophoneIpcRequest::Status,
        [arg] if arg == "--kill" => MicrophoneIpcRequest::Kill,
        [arg] if arg == "--enable" => MicrophoneIpcRequest::Enable,
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "Usage:\n  focald-mic --start\n  focald-mic --stop\n  focald-mic --toggle\n  focald-mic --status\n  focald-mic --kill\n  focald-mic --enable"
            );
            return Ok(());
        }
        _ => anyhow::bail!(
            "usage: focald-mic --start | --stop | --toggle | --status | --kill | --enable"
        ),
    };
    let request = serde_json::to_string(&request)?;
    let response = send_socket_request(mic_socket_path()?, &request, Duration::from_secs(5))?;
    print!("{response}");
    Ok(())
}

fn runtime_socket(name: &str, override_name: &str) -> Result<PathBuf> {
    transport::socket_path(override_name, name).map_err(anyhow::Error::msg)
}

fn mic_socket_path() -> Result<PathBuf> {
    runtime_socket("focald-mic.sock", "FOCALD_MIC_SOCKET")
}

fn voice_socket_path() -> Result<PathBuf> {
    runtime_socket("focald-voice.sock", "FOCALD_VOICE_SOCKET")
}

fn speech_socket_path() -> Result<PathBuf> {
    runtime_socket("focald-speech.sock", "FOCALD_SPEECH_SOCKET")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_decode() {
        assert_eq!(
            decode_request(r#"{"command":"start"}"#).unwrap(),
            MicrophoneIpcRequest::Start {
                requester: "focaldesk-desktop".into()
            }
        );
        assert_eq!(
            decode_request(r#"{"command":"stop"}"#).unwrap(),
            MicrophoneIpcRequest::Stop { lease_id: None }
        );
        assert_eq!(
            decode_request(r#"{"command":"toggle"}"#).unwrap(),
            MicrophoneIpcRequest::Toggle {
                requester: "focaldesk-desktop".into()
            }
        );
        assert_eq!(
            decode_request(r#"{"command":"status"}"#).unwrap(),
            MicrophoneIpcRequest::Status
        );
    }

    #[test]
    fn malformed_commands_are_rejected() {
        assert!(decode_request(r#"{"command":"listen"}"#).is_err());
        assert!(decode_request(r#"{"command":"start","extra":true}"#).is_err());
        assert!(decode_request("").is_err());
    }

    #[test]
    fn idle_state_reports_idle_and_stops_idempotently() {
        let mut state = MicState::default();
        assert_eq!(state.status(), "idle");
        assert_eq!(state.stop(None).unwrap().status, "idle");
    }

    #[test]
    fn lease_events_are_bounded_and_cursor_filtered() {
        let mut state = MicState {
            lease: Some(MicLease {
                id: "lease-1".into(),
                requester: "test-client".into(),
                ambient: true,
                last_heartbeat: Instant::now(),
            }),
            ..MicState::default()
        };
        for index in 0..(MAX_EVENTS + 10) {
            state.record(MicrophoneEvent::Command(format!("command-{index}")));
        }
        assert_eq!(state.events_log.len(), MAX_EVENTS);
        let after = state.events_log[state.events_log.len() - 2].sequence;
        let response = state.poll("lease-1", after).unwrap();
        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].sequence, response.latest_sequence);
    }

    #[test]
    fn mismatched_lease_cannot_poll_or_stop() {
        let mut state = MicState {
            lease: Some(MicLease {
                id: "lease-1".into(),
                requester: "test-client".into(),
                ambient: true,
                last_heartbeat: Instant::now(),
            }),
            ..MicState::default()
        };
        assert!(state.poll("lease-2", 0).is_err());
        assert!(state.stop(Some("lease-2")).is_err());
    }
}
