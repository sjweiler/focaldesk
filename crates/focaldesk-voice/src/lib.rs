use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;
use vosk::{DecodingState, Model, Recognizer};

pub const DEFAULT_MODEL_DIR_NAME: &str = "vosk-model-small-en-us-0.15";

/// An event streamed from a running [`VoiceSession`] as speech is recognized.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// The input stream is open and microphone samples are being captured.
    Ready,
    /// In-progress recognition of the current phrase. Replaces the previous `Partial`.
    Partial(String),
    /// A finalized phrase (silence was detected after it). Should be appended permanently.
    Final(String),
    /// Recognition stopped because of an error; the session has ended.
    Error(String),
}

/// Looks for an installed Vosk model directory, checking in order:
/// - the `FOCALDESK_VOSK_MODEL_DIR` env var
/// - `$XDG_DATA_HOME/focaldesk/voice/vosk-model-small-en-us-0.15`
pub fn find_model_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("FOCALDESK_VOSK_MODEL_DIR") {
        let path = PathBuf::from(dir);
        if is_model_dir(&path) {
            return Some(path);
        }
    }

    let candidate = dirs::data_dir()?
        .join("focaldesk")
        .join("voice")
        .join(DEFAULT_MODEL_DIR_NAME);
    is_model_dir(&candidate).then_some(candidate)
}

fn is_model_dir(path: &Path) -> bool {
    path.join("am").join("final.mdl").is_file()
}

/// Message shown to the user when no offline speech model is installed.
pub fn install_instructions() -> String {
    format!(
        "No offline speech model found. Install one with:\n\
         mkdir -p ~/.local/share/focaldesk/voice && cd ~/.local/share/focaldesk/voice && \
         curl -LO https://alphacephei.com/vosk/models/{name}.zip && unzip {name}.zip",
        name = DEFAULT_MODEL_DIR_NAME
    )
}

/// A running voice-recognition session that captures microphone audio and streams
/// recognized text back through the channel passed to [`VoiceSession::start`].
pub struct VoiceSession {
    stop: Arc<AtomicBool>,
}

impl VoiceSession {
    /// Starts capturing microphone audio and recognizing speech, sending [`VoiceEvent`]s
    /// as they occur. Runs on a background thread until [`stop`](Self::stop) is called
    /// or a fatal error occurs.
    pub fn start(model_dir: PathBuf, events: Sender<VoiceEvent>) -> Result<Self> {
        if !is_model_dir(&model_dir) {
            return Err(anyhow!("{}", install_instructions()));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();

        thread::Builder::new()
            .name("focaldesk-voice".into())
            .spawn(move || {
                if let Err(err) = run_recognition(&model_dir, &stop_for_thread, &events) {
                    let _ = events.send(VoiceEvent::Error(err.to_string()));
                }
            })
            .context("failed to spawn voice recognition thread")?;

        Ok(Self { stop })
    }

    /// Signals the recognition session to stop. Capture winds down asynchronously;
    /// the last recognized phrase (if any) still arrives as a [`VoiceEvent::Final`]
    /// shortly after this call.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn run_recognition(model_dir: &Path, stop: &AtomicBool, events: &Sender<VoiceEvent>) -> Result<()> {
    vosk::set_log_level(vosk::LogLevel::Error);

    let model = Model::new(model_dir.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow!("failed to load speech model at {}", model_dir.display()))?;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no microphone found"))?;
    let config = device
        .default_input_config()
        .context("microphone has no usable input configuration")?;

    let sample_rate = config.sample_rate() as f32;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let mut recognizer = Recognizer::new(&model, sample_rate)
        .ok_or_else(|| anyhow!("failed to create speech recognizer"))?;
    recognizer.set_partial_words(false);

    let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
    let err_fn = |err| eprintln!("focaldesk-voice: audio stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(downmix_f32(data, channels));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(downmix_i16(data, channels));
            },
            err_fn,
            None,
        ),
        other => return Err(anyhow!("unsupported microphone sample format: {other:?}")),
    }
    .context("failed to open microphone stream")?;

    stream
        .play()
        .context("failed to start microphone capture")?;
    let _ = events.send(VoiceEvent::Ready);

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => match recognizer.accept_waveform(&chunk) {
                Ok(DecodingState::Finalized) => {
                    let text = recognizer
                        .result()
                        .single()
                        .map(|r| r.text.to_string())
                        .unwrap_or_default();
                    if !text.is_empty() {
                        let _ = events.send(VoiceEvent::Final(text));
                    }
                }
                Ok(DecodingState::Running) => {
                    let partial = recognizer.partial_result();
                    if !partial.partial.is_empty() {
                        let _ = events.send(VoiceEvent::Partial(partial.partial.to_string()));
                    }
                }
                Ok(DecodingState::Failed) | Err(_) => {}
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream);

    let final_text = recognizer
        .final_result()
        .single()
        .map(|r| r.text.to_string())
        .unwrap_or_default();
    if !final_text.is_empty() {
        let _ = events.send(VoiceEvent::Final(final_text));
    }

    Ok(())
}

fn downmix_f32(data: &[f32], channels: usize) -> Vec<i16> {
    data.chunks(channels.max(1))
        .map(|frame| {
            let avg = frame.iter().sum::<f32>() / frame.len() as f32;
            (avg.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })
        .collect()
}

fn downmix_i16(data: &[i16], channels: usize) -> Vec<i16> {
    data.chunks(channels.max(1))
        .map(|frame| (frame.iter().map(|&s| s as i32).sum::<i32>() / frame.len() as i32) as i16)
        .collect()
}
