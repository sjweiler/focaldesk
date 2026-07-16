//! focald-voice: voice text -> intent -> typed IPC action.
//!
//! Reads one utterance per line on stdin (pipe your STT engine's output in),
//! runs the pipeline, and writes the resulting action to the compositor
//! socket. Swap stdin for a Unix socket listener if the STT engine lives in
//! its own process — the pipeline function doesn't change.
//!
//!   whisper-stream ... | focald-voice

mod action;
mod fastpath;
mod intent;
mod ipc;
mod llm;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::Result;

use action::{to_action, CompositorState, MapError};
use intent::VoiceIntent;
use ipc::IpcClient;
use llm::PromptContext;

fn main() -> Result<()> {
    let app_settings = focaldesk_settings_core::load_settings().apps;
    // Follow up by querying the compositor for live outputs and hot-reloading
    // app settings instead of taking this startup snapshot.
    let state = CompositorState {
        output_count: 3,
        apps: HashMap::from([
            ("firefox".into(), "firefox".into()),
            ("browser".into(), app_settings.browser),
            ("terminal".into(), app_settings.terminal),
            ("kitty".into(), "kitty".into()),
            ("files".into(), app_settings.file_manager),
            ("settings".into(), "focaldesk-settings".into()),
        ]),
    };

    let prompt_ctx = PromptContext {
        outputs: vec![
            (0, "DP-1".into()),
            (1, "DP-2".into()),
            (2, "HDMI-A-1".into()),
        ],
        known_apps: state.apps.keys().cloned().collect(),
    };

    if std::env::args().any(|arg| arg == "--stdin") {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let text = line?;
            match process_text(&text, &state, &prompt_ctx) {
                Ok(desc) => eprintln!("[ok] {:?} -> {desc}", text.trim()),
                Err(err) => eprintln!("[rejected] {:?}: {err}", text.trim()),
            }
        }
        return Ok(());
    }

    let socket = voice_socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("focald-voice: listening at {}", socket.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_client(stream, &state, &prompt_ctx),
            Err(err) => eprintln!("focald-voice: accept failed: {err}"),
        }
    }

    Ok(())
}

fn voice_socket_path() -> PathBuf {
    std::env::var_os("FOCALD_VOICE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            runtime.join("focald-voice.sock")
        })
}

fn handle_client(mut stream: UnixStream, state: &CompositorState, prompt_ctx: &PromptContext) {
    let mut text = String::new();
    if let Err(err) = BufReader::new(&stream)
        .take(64 * 1024)
        .read_to_string(&mut text)
    {
        let _ = writeln!(stream, "error: reading request: {err}");
        return;
    }

    match process_text(&text, state, prompt_ctx) {
        Ok(desc) => {
            eprintln!("[ok] {:?} -> {desc}", text.trim());
            let _ = writeln!(stream, "ok: {desc}");
        }
        Err(err) => {
            eprintln!("[rejected] {:?}: {err}", text.trim());
            let _ = writeln!(stream, "error: {err}");
        }
    }
}

fn process_text(
    text: &str,
    state: &CompositorState,
    prompt_ctx: &PromptContext,
) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty utterance".into());
    }
    let mut client = IpcClient::connect().map_err(|err| format!("ipc: {err:#}"))?;
    handle_utterance(text, state, prompt_ctx, &mut client)
}

fn handle_utterance(
    text: &str,
    state: &CompositorState,
    prompt_ctx: &PromptContext,
    client: &mut IpcClient,
) -> Result<String, String> {
    // 1. Fast path: exact phrasings skip inference entirely.
    // 2. Slow path: schema-constrained LLM + serde gate.
    let intent: VoiceIntent = match fastpath::try_match(text) {
        Some(intent) => intent,
        None => llm::extract_intent(text, prompt_ctx).map_err(|e| format!("llm: {e:#}"))?,
    };

    // 3. Trusted mapping: intent -> validated typed action.
    let action = to_action(intent, state).map_err(|e| match e {
        MapError::UnknownApp(a) => format!("no app called {a:?}"),
        MapError::BadOutput(o) => format!("output {o} doesn't exist"),
        MapError::BadValue(v) => format!("out-of-range value: {v}"),
        MapError::Unrecognized(raw) => format!("didn't understand: {raw:?}"),
    })?;

    let desc = format!("{action:?}");

    // 4. Wire: bincode frame over the compositor socket.
    client.send(action).map_err(|e| format!("ipc: {e:#}"))?;

    Ok(desc)
}
