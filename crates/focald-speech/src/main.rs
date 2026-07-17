//! Local text-to-speech daemon for the active FocalDesk session.

use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_TEXT_CHARS: usize = 4096;
const COMMAND_BUFFER: usize = 32;
const MAX_PENDING_SPEECH: usize = 32;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Priority {
    #[default]
    Normal,
    Interrupt,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CommandKind {
    Speak,
    Stop,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    #[serde(default)]
    command: Option<CommandKind>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    priority: Priority,
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Serialize)]
struct WireResponse<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug)]
struct SpeechJob {
    text: String,
    priority: Priority,
    replace: bool,
}

#[derive(Debug)]
enum WorkerCommand {
    Speak(SpeechJob),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Espeak,
    Piper,
}

impl BackendKind {
    fn from_env() -> Result<Self> {
        Self::parse(
            &std::env::var("FOCALD_SPEECH_BACKEND").unwrap_or_else(|_| "espeak-ng".to_string()),
        )
    }

    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "espeak" | "espeak-ng" => Ok(Self::Espeak),
            "piper" => Ok(Self::Piper),
            backend => anyhow::bail!(
                "unsupported FOCALD_SPEECH_BACKEND {backend:?}; expected espeak-ng or piper"
            ),
        }
    }

    fn default_program(self) -> &'static str {
        match self {
            Self::Espeak => "espeak-ng",
            Self::Piper => "piper",
        }
    }
}

struct SpeechBackend {
    kind: BackendKind,
    program: String,
    voice: Option<String>,
    piper_model: Option<String>,
    piper_sample_rate: u32,
    player: String,
    rate: u16,
    amplitude: u16,
}

impl SpeechBackend {
    fn from_env() -> Result<Self> {
        let kind = BackendKind::from_env()?;
        let backend = Self {
            kind,
            program: std::env::var("FOCALD_SPEECH_PROGRAM")
                .unwrap_or_else(|_| kind.default_program().to_string()),
            voice: std::env::var("FOCALD_SPEECH_VOICE")
                .ok()
                .filter(|voice| !voice.trim().is_empty()),
            piper_model: std::env::var("FOCALD_SPEECH_PIPER_MODEL")
                .ok()
                .filter(|model| !model.trim().is_empty()),
            piper_sample_rate: env_number_u32(
                "FOCALD_SPEECH_PIPER_SAMPLE_RATE",
                22_050,
                8_000,
                192_000,
            ),
            player: std::env::var("FOCALD_SPEECH_PLAYER").unwrap_or_else(|_| "pw-play".to_string()),
            rate: env_number("FOCALD_SPEECH_RATE", 175, 80, 450),
            amplitude: env_number("FOCALD_SPEECH_AMPLITUDE", 100, 0, 200),
        };
        if backend.kind == BackendKind::Piper && backend.piper_model.is_none() {
            anyhow::bail!(
                "FOCALD_SPEECH_PIPER_MODEL must name a Piper voice or .onnx model when using Piper"
            );
        }
        Ok(backend)
    }

    fn spawn(&self, text: &str) -> Result<BackendProcess> {
        match self.kind {
            BackendKind::Espeak => self.spawn_espeak(text),
            BackendKind::Piper => self.spawn_piper(text),
        }
    }

    fn spawn_espeak(&self, text: &str) -> Result<BackendProcess> {
        let mut command = Command::new(&self.program);
        command
            .args([
                "--stdin",
                "-s",
                &self.rate.to_string(),
                "-a",
                &self.amplitude.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(voice) = &self.voice {
            command.args(["-v", voice]);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("start TTS backend {:?}", self.program))?;
        child
            .stdin
            .take()
            .context("TTS backend stdin was unavailable")?
            .write_all(text.as_bytes())
            .context("write text to TTS backend")?;
        Ok(BackendProcess::new(vec![("espeak-ng", child)]))
    }

    fn spawn_piper(&self, text: &str) -> Result<BackendProcess> {
        let mut command = self.piper_command()?;
        let mut piper = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start Piper backend {:?}", self.program))?;

        let audio = piper
            .stdout
            .take()
            .context("Piper stdout was unavailable")?;
        let player = match Command::new(&self.player)
            .args([
                "--raw",
                "--rate",
                &self.piper_sample_rate.to_string(),
                "--channels",
                "1",
                "--format",
                "s16",
                "-",
            ])
            .stdin(Stdio::from(audio))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start Piper audio player {:?}", self.player))
        {
            Ok(player) => player,
            Err(err) => {
                let _ = piper.kill();
                let _ = piper.wait();
                return Err(err);
            }
        };

        let input = piper.stdin.take();
        let mut process = BackendProcess::new(vec![("piper", piper), ("player", player)]);
        let write_result = input
            .context("Piper stdin was unavailable")
            .and_then(|mut input| {
                input
                    .write_all(text.as_bytes())
                    .context("write text to Piper")
            });
        if let Err(err) = write_result {
            process.cancel();
            return Err(err);
        }

        Ok(process)
    }

    fn piper_command(&self) -> Result<Command> {
        let model = self
            .piper_model
            .as_deref()
            .context("Piper model was not configured")?;
        let length_scale = format!("{:.3}", 175.0 / f64::from(self.rate));
        let volume = format!("{:.2}", f64::from(self.amplitude) / 100.0);
        let mut command = Command::new(&self.program);
        command.args([
            "--model",
            model,
            "--output-raw",
            "--length-scale",
            &length_scale,
            "--volume",
            &volume,
        ]);
        Ok(command)
    }
}

struct BackendChild {
    name: &'static str,
    child: Child,
    status: Option<ExitStatus>,
}

struct BackendProcess {
    children: Vec<BackendChild>,
}

impl BackendProcess {
    fn new(children: Vec<(&'static str, Child)>) -> Self {
        Self {
            children: children
                .into_iter()
                .map(|(name, child)| BackendChild {
                    name,
                    child,
                    status: None,
                })
                .collect(),
        }
    }

    fn try_wait(&mut self) -> Result<Option<()>> {
        for process in &mut self.children {
            if process.status.is_none() {
                process.status = process
                    .child
                    .try_wait()
                    .with_context(|| format!("waiting for {}", process.name))?;
            }
        }
        if self.children.iter().any(|process| process.status.is_none()) {
            return Ok(None);
        }
        for process in &self.children {
            let status = process.status.expect("all child statuses were collected");
            if !status.success() {
                anyhow::bail!("{} exited with {status}", process.name);
            }
        }
        Ok(Some(()))
    }

    fn cancel(&mut self) {
        for process in &mut self.children {
            if process.status.is_none() {
                let _ = process.child.kill();
            }
        }
        for process in &mut self.children {
            if process.status.is_none() {
                let _ = process.child.wait();
            }
        }
    }
}

fn env_number(name: &str, default: u16, min: u16, max: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(default)
}

fn env_number_u32(name: &str, default: u32, min: u32, max: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(default)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return run_client(&args);
    }
    run_server()
}

fn run_server() -> Result<()> {
    let socket = speech_socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("bind speech socket {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    let (commands_tx, commands_rx) = mpsc::sync_channel(COMMAND_BUFFER);
    let backend = SpeechBackend::from_env().context("configure speech backend")?;
    std::thread::Builder::new()
        .name("focald-speech-worker".into())
        .spawn(move || worker_loop(commands_rx, backend))
        .context("start speech worker")?;

    eprintln!("focald-speech: listening at {}", socket.display());
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_client(stream, &commands_tx),
            Err(err) => eprintln!("focald-speech: accept failed: {err}"),
        }
    }
    Ok(())
}

fn handle_client(mut stream: UnixStream, commands: &SyncSender<WorkerCommand>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut payload = String::new();
    let result = BufReader::new(&stream)
        .take(MAX_REQUEST_BYTES)
        .read_to_string(&mut payload)
        .map_err(|err| format!("reading request: {err}"))
        .and_then(|_| decode_request(&payload))
        .and_then(|command| {
            commands.try_send(command).map_err(|err| match err {
                TrySendError::Full(_) => "speech queue is full".to_string(),
                TrySendError::Disconnected(_) => "speech worker is unavailable".to_string(),
            })
        });

    let response = match result {
        Ok(()) => WireResponse {
            status: "accepted",
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

fn decode_request(payload: &str) -> Result<WorkerCommand, String> {
    let request: WireRequest =
        serde_json::from_str(payload).map_err(|err| format!("invalid JSON request: {err}"))?;
    match request.command {
        Some(CommandKind::Stop) => {
            if request.text.is_some() {
                return Err("stop requests must not include text".into());
            }
            Ok(WorkerCommand::Stop)
        }
        None | Some(CommandKind::Speak) => {
            let text = request
                .text
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .ok_or_else(|| "speech text is empty".to_string())?;
            if text.chars().count() > MAX_TEXT_CHARS {
                return Err(format!("speech text exceeds {MAX_TEXT_CHARS} characters"));
            }
            Ok(WorkerCommand::Speak(SpeechJob {
                text,
                priority: request.priority,
                replace: request.replace,
            }))
        }
    }
}

fn worker_loop(commands: Receiver<WorkerCommand>, backend: SpeechBackend) {
    let mut pending = VecDeque::<SpeechJob>::new();
    let mut current: Option<(BackendProcess, String)> = None;

    loop {
        if current.is_none() {
            while let Some(job) = pending.pop_front() {
                let description = summarize(&job.text);
                match backend.spawn(&job.text) {
                    Ok(child) => {
                        eprintln!("[speaking] {description:?}");
                        current = Some((child, description));
                        break;
                    }
                    Err(err) => eprintln!("[failed] {description:?}: {err:#}"),
                }
            }
        }

        let command = if current.is_some() {
            match commands.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };

        if let Some(command) = command {
            match command {
                WorkerCommand::Stop => {
                    pending.clear();
                    cancel_current(&mut current);
                    eprintln!("[stopped]");
                }
                WorkerCommand::Speak(job) if job.priority == Priority::Interrupt => {
                    pending.clear();
                    cancel_current(&mut current);
                    pending.push_front(job);
                }
                WorkerCommand::Speak(job) => {
                    if job.replace {
                        pending.clear();
                    }
                    if pending.len() == MAX_PENDING_SPEECH {
                        pending.pop_front();
                        eprintln!("focald-speech: dropped oldest queued utterance");
                    }
                    pending.push_back(job);
                }
            }
        }

        if let Some((process, description)) = current.as_mut() {
            match process.try_wait() {
                Ok(Some(())) => {
                    eprintln!("[finished] {description:?}");
                    current = None;
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("[failed] {description:?}: {err:#}");
                    process.cancel();
                    current = None;
                }
            }
        }
    }

    cancel_current(&mut current);
}

fn cancel_current(current: &mut Option<(BackendProcess, String)>) {
    if let Some((mut process, description)) = current.take() {
        process.cancel();
        eprintln!("[cancelled] {description:?}");
    }
}

fn summarize(text: &str) -> String {
    const LIMIT: usize = 80;
    let mut summary: String = text.chars().take(LIMIT).collect();
    if text.chars().count() > LIMIT {
        summary.push('…');
    }
    summary
}

fn run_client(args: &[String]) -> Result<()> {
    let request = match args.first().map(String::as_str) {
        Some("--speak") if args.len() > 1 => serde_json::json!({
            "command": "speak",
            "text": args[1..].join(" ")
        }),
        Some("--interrupt") if args.len() > 1 => serde_json::json!({
            "command": "speak",
            "text": args[1..].join(" "),
            "priority": "interrupt"
        }),
        Some("--stop") if args.len() == 1 => serde_json::json!({ "command": "stop" }),
        Some("--help" | "-h") => {
            println!(
                "Usage:\n  focald-speech --speak TEXT\n  focald-speech --interrupt TEXT\n  focald-speech --stop"
            );
            return Ok(());
        }
        _ => anyhow::bail!("usage: focald-speech --speak TEXT | --interrupt TEXT | --stop"),
    };

    let socket = speech_socket_path();
    let mut stream =
        UnixStream::connect(&socket).with_context(|| format!("connect to {}", socket.display()))?;
    serde_json::to_writer(&mut stream, &request)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    print!("{response}");
    Ok(())
}

fn speech_socket_path() -> PathBuf {
    std::env::var_os("FOCALD_SPEECH_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            runtime.join("focald-speech.sock")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_parsed() {
        assert_eq!(BackendKind::parse("espeak").unwrap(), BackendKind::Espeak);
        assert_eq!(
            BackendKind::parse("ESPEAK-NG").unwrap(),
            BackendKind::Espeak
        );
        assert_eq!(BackendKind::parse("Piper").unwrap(), BackendKind::Piper);
        assert!(BackendKind::parse("festival").is_err());
    }

    #[test]
    fn piper_command_uses_model_rate_and_volume() {
        let backend = SpeechBackend {
            kind: BackendKind::Piper,
            program: "piper-custom".into(),
            voice: None,
            piper_model: Some("voice.onnx".into()),
            piper_sample_rate: 22_050,
            player: "pw-play".into(),
            rate: 350,
            amplitude: 80,
        };
        let command = backend.piper_command().unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "piper-custom");
        assert_eq!(
            args,
            [
                "--model",
                "voice.onnx",
                "--output-raw",
                "--length-scale",
                "0.500",
                "--volume",
                "0.80"
            ]
        );
    }

    #[test]
    fn text_only_request_defaults_to_normal_speech() {
        let command = decode_request(r#"{"text":"  hello world  "}"#).unwrap();
        let WorkerCommand::Speak(job) = command else {
            panic!("expected speech job");
        };
        assert_eq!(job.text, "hello world");
        assert_eq!(job.priority, Priority::Normal);
        assert!(!job.replace);
    }

    #[test]
    fn interrupt_request_is_recognized() {
        let command =
            decode_request(r#"{"command":"speak","text":"warning","priority":"interrupt"}"#)
                .unwrap();
        let WorkerCommand::Speak(job) = command else {
            panic!("expected speech job");
        };
        assert_eq!(job.priority, Priority::Interrupt);
    }

    #[test]
    fn stop_request_is_recognized() {
        assert!(matches!(
            decode_request(r#"{"command":"stop"}"#).unwrap(),
            WorkerCommand::Stop
        ));
    }

    #[test]
    fn empty_and_unknown_requests_are_rejected() {
        assert!(decode_request(r#"{"text":"  "}"#).is_err());
        assert!(decode_request(r#"{"wat":true}"#).is_err());
    }
}
