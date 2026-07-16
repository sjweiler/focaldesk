//! Push-to-talk microphone and speech-to-text daemon for FocalDesk.

use std::io::{BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use focaldesk_voice::{VoiceEvent, VoiceSession};
use serde::{Deserialize, Serialize};

const MAX_REQUEST_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MicCommand {
    Start,
    Stop,
    Toggle,
    Status,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    command: MicCommand,
}

#[derive(Debug, Serialize)]
struct WireResponse<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Default)]
struct MicState {
    session: Option<VoiceSession>,
    events: Option<Receiver<VoiceEvent>>,
    transcript_parts: Vec<String>,
    ready: bool,
    stopping: bool,
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

    fn start(&mut self) -> Result<&'static str, String> {
        if self.session.is_some() {
            return if self.stopping {
                Err("microphone capture is still stopping".into())
            } else {
                Ok(self.status())
            };
        }

        let model_dir =
            focaldesk_voice::find_model_dir().ok_or_else(focaldesk_voice::install_instructions)?;
        stop_speech();
        let (events_tx, events_rx) = mpsc::channel();
        let session = VoiceSession::start(model_dir, events_tx)
            .map_err(|err| format!("start microphone capture: {err:#}"))?;
        self.session = Some(session);
        self.events = Some(events_rx);
        self.transcript_parts.clear();
        self.ready = false;
        self.stopping = false;
        eprintln!("[starting]");
        Ok("starting")
    }

    fn stop(&mut self) -> &'static str {
        if let Some(session) = &self.session {
            session.stop();
            self.stopping = true;
            eprintln!("[stopping]");
            "stopping"
        } else {
            "idle"
        }
    }

    fn toggle(&mut self) -> Result<&'static str, String> {
        if self.session.is_some() {
            Ok(self.stop())
        } else {
            self.start()
        }
    }

    fn poll_events(&mut self) {
        loop {
            let event = match self.events.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Empty)) | None => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    let transcript = std::mem::take(&mut self.transcript_parts).join(" ");
                    let should_forward = self.stopping && !transcript.trim().is_empty();
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
                    eprintln!("[listening]");
                }
                VoiceEvent::Partial(_) => {}
                VoiceEvent::Final(text) if !text.trim().is_empty() => {
                    let text = text.trim().to_string();
                    eprintln!("[transcript-part] {text:?}");
                    self.transcript_parts.push(text);
                }
                VoiceEvent::Final(_) => {}
                VoiceEvent::Error(message) => {
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
    let socket = mic_socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("bind microphone socket {}", socket.display()))?;
    listener.set_nonblocking(true)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("focald-mic: listening at {}", socket.display());

    let mut state = MicState::default();
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => handle_client(stream, &mut state),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => eprintln!("focald-mic: accept failed: {err}"),
            }
        }
        state.poll_events();
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn handle_client(mut stream: UnixStream, state: &mut MicState) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut payload = String::new();
    let result = BufReader::new(&stream)
        .take(MAX_REQUEST_BYTES)
        .read_to_string(&mut payload)
        .map_err(|err| format!("reading request: {err}"))
        .and_then(|_| decode_request(&payload))
        .and_then(|command| execute_command(command, state));

    let response = match result {
        Ok(status) => WireResponse {
            status,
            message: None,
        },
        Err(message) => WireResponse {
            status: "error",
            message: Some(message),
        },
    };
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = writeln!(stream);
}

fn decode_request(payload: &str) -> Result<MicCommand, String> {
    serde_json::from_str::<WireRequest>(payload)
        .map(|request| request.command)
        .map_err(|err| format!("invalid JSON request: {err}"))
}

fn execute_command(command: MicCommand, state: &mut MicState) -> Result<&'static str, String> {
    match command {
        MicCommand::Start => state.start(),
        MicCommand::Stop => Ok(state.stop()),
        MicCommand::Toggle => state.toggle(),
        MicCommand::Status => Ok(state.status()),
    }
}

fn stop_speech() {
    let request = serde_json::json!({ "command": "stop" }).to_string();
    if let Err(err) = send_socket_request(speech_socket_path(), &request, Duration::from_secs(2)) {
        eprintln!("focald-mic: could not stop speech playback: {err:#}");
    }
}

fn forward_transcript(text: String) {
    let _ = std::thread::Builder::new()
        .name("focald-mic-forward".into())
        .spawn(move || {
            match send_socket_request(voice_socket_path(), &text, Duration::from_secs(20)) {
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
    stream.write_all(payload.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn run_client(args: &[String]) -> Result<()> {
    let command = match args {
        [arg] if arg == "--start" => "start",
        [arg] if arg == "--stop" => "stop",
        [arg] if arg == "--toggle" => "toggle",
        [arg] if arg == "--status" => "status",
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "Usage:\n  focald-mic --start\n  focald-mic --stop\n  focald-mic --toggle\n  focald-mic --status"
            );
            return Ok(());
        }
        _ => anyhow::bail!("usage: focald-mic --start | --stop | --toggle | --status"),
    };
    let request = serde_json::json!({ "command": command }).to_string();
    let response = send_socket_request(mic_socket_path(), &request, Duration::from_secs(5))?;
    print!("{response}");
    Ok(())
}

fn runtime_socket(name: &str, override_name: &str) -> PathBuf {
    std::env::var_os(override_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(name)
        })
}

fn mic_socket_path() -> PathBuf {
    runtime_socket("focald-mic.sock", "FOCALD_MIC_SOCKET")
}

fn voice_socket_path() -> PathBuf {
    runtime_socket("focald-voice.sock", "FOCALD_VOICE_SOCKET")
}

fn speech_socket_path() -> PathBuf {
    runtime_socket("focald-speech.sock", "FOCALD_SPEECH_SOCKET")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_decode() {
        assert_eq!(
            decode_request(r#"{"command":"start"}"#).unwrap(),
            MicCommand::Start
        );
        assert_eq!(
            decode_request(r#"{"command":"stop"}"#).unwrap(),
            MicCommand::Stop
        );
        assert_eq!(
            decode_request(r#"{"command":"toggle"}"#).unwrap(),
            MicCommand::Toggle
        );
        assert_eq!(
            decode_request(r#"{"command":"status"}"#).unwrap(),
            MicCommand::Status
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
        assert_eq!(state.stop(), "idle");
    }
}
