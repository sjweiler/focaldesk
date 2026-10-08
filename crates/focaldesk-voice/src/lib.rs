use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};
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
    /// Voice activity changed according to the local energy detector.
    VoiceActivity(bool),
    /// The configured local wake phrase was recognized.
    WakeDetected,
    /// A finalized ambient command after wake-phrase gating.
    Command(String),
    /// Capture stopped and its in-memory rolling audio was discarded.
    Stopped,
    /// Recognition stopped because of an error; the session has ended.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct AmbientVoiceConfig {
    pub wake_phrase: String,
    pub command_window: Duration,
    pub rolling_buffer: Duration,
    pub activity_threshold: f32,
    pub requester_application: String,
    pub blocked_applications: Vec<String>,
}

impl Default for AmbientVoiceConfig {
    fn default() -> Self {
        Self {
            wake_phrase: "hello focaldesk".into(),
            command_window: Duration::from_secs(10),
            rolling_buffer: Duration::from_secs(3),
            activity_threshold: 0.015,
            requester_application: "focaldesk-ai-console".into(),
            blocked_applications: Vec::new(),
        }
    }
}

static MICROPHONE_KILLED: AtomicBool = AtomicBool::new(false);

pub fn set_microphone_killed(killed: bool) {
    MICROPHONE_KILLED.store(killed, Ordering::SeqCst);
}

pub fn microphone_killed() -> bool {
    MICROPHONE_KILLED.load(Ordering::SeqCst)
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

impl Drop for VoiceSession {
    fn drop(&mut self) {
        self.stop();
    }
}

impl VoiceSession {
    /// Starts capturing microphone audio and recognizing speech, sending [`VoiceEvent`]s
    /// as they occur. Runs on a background thread until [`stop`](Self::stop) is called
    /// or a fatal error occurs.
    pub fn start(model_dir: PathBuf, events: Sender<VoiceEvent>) -> Result<Self> {
        Self::start_inner(model_dir, events, None)
    }

    pub fn start_ambient(
        model_dir: PathBuf,
        events: Sender<VoiceEvent>,
        config: AmbientVoiceConfig,
    ) -> Result<Self> {
        validate_ambient_config(&config)?;
        if config
            .blocked_applications
            .iter()
            .any(|application| application == &config.requester_application)
        {
            return Err(anyhow!("microphone access is blocked for this application"));
        }
        Self::start_inner(model_dir, events, Some(config))
    }

    fn start_inner(
        model_dir: PathBuf,
        events: Sender<VoiceEvent>,
        config: Option<AmbientVoiceConfig>,
    ) -> Result<Self> {
        if !is_model_dir(&model_dir) {
            return Err(anyhow!("{}", install_instructions()));
        }
        if microphone_killed() {
            return Err(anyhow!("microphone kill switch is active"));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();

        thread::Builder::new()
            .name("focaldesk-voice".into())
            .spawn(move || {
                if let Err(err) = run_recognition(&model_dir, &stop_for_thread, &events, config) {
                    let _ = events.send(VoiceEvent::Error(err.to_string()));
                }
            })
            .context("failed to spawn voice recognition thread")?;

        Ok(Self { stop })
    }

    /// Signals the recognition session to stop. Capture winds down asynchronously
    /// and emits [`VoiceEvent::Stopped`] after discarding buffered audio.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn run_recognition(
    model_dir: &Path,
    stop: &AtomicBool,
    events: &Sender<VoiceEvent>,
    ambient: Option<AmbientVoiceConfig>,
) -> Result<()> {
    vosk::set_log_level(vosk::LogLevel::Error);

    let model = Model::new(model_dir.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow!("failed to load speech model at {}", model_dir.display()))?;
    if stop.load(Ordering::Relaxed) || microphone_killed() {
        let _ = events.send(VoiceEvent::Stopped);
        return Ok(());
    }

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

    // The callback must never create an unbounded audio backlog. If recognition
    // falls behind, dropping an old-sized chunk is safer than retaining raw audio.
    let (tx, rx) = sync_channel::<Vec<i16>>(8);
    let err_fn = |err| eprintln!("focaldesk-voice: audio stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(downmix_f32(data, channels));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(downmix_i16(data, channels));
            },
            err_fn,
            None,
        ),
        other => return Err(anyhow!("unsupported microphone sample format: {other:?}")),
    }
    .context("failed to open microphone stream")?;

    if stop.load(Ordering::Relaxed) || microphone_killed() {
        let _ = events.send(VoiceEvent::Stopped);
        return Ok(());
    }
    stream
        .play()
        .context("failed to start microphone capture")?;
    let _ = events.send(VoiceEvent::Ready);
    let max_rolling_samples = ambient
        .as_ref()
        .map(|config| (sample_rate as f64 * config.rolling_buffer.as_secs_f64()) as usize)
        .unwrap_or(0);
    let mut rolling_audio = VecDeque::<i16>::with_capacity(max_rolling_samples);
    let mut voice_active = false;
    let mut command_until: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) && !microphone_killed() {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                if let Some(config) = &ambient {
                    let active = rms(&chunk) >= config.activity_threshold;
                    if active != voice_active {
                        voice_active = active;
                        let _ = events.send(VoiceEvent::VoiceActivity(active));
                    }
                    rolling_audio.extend(chunk.iter().copied());
                    while rolling_audio.len() > max_rolling_samples {
                        rolling_audio.pop_front();
                    }
                }
                match recognizer.accept_waveform(&chunk) {
                    Ok(DecodingState::Finalized) => {
                        let text = recognizer
                            .result()
                            .single()
                            .map(|r| r.text.to_string())
                            .unwrap_or_default();
                        if !text.is_empty() {
                            emit_recognized(&text, ambient.as_ref(), &mut command_until, events);
                        }
                    }
                    Ok(DecodingState::Running) => {
                        let partial = recognizer.partial_result();
                        let within_command_window =
                            command_until.is_some_and(|deadline| Instant::now() <= deadline);
                        if !partial.partial.is_empty()
                            && (ambient.is_none() || within_command_window)
                        {
                            let _ = events.send(VoiceEvent::Partial(partial.partial.to_string()));
                        }
                    }
                    Ok(DecodingState::Failed) | Err(_) => {}
                }
            }
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
        emit_recognized(&final_text, ambient.as_ref(), &mut command_until, events);
    }
    rolling_audio.clear();
    let _ = events.send(VoiceEvent::Stopped);

    Ok(())
}

fn validate_ambient_config(config: &AmbientVoiceConfig) -> Result<()> {
    if config.wake_phrase.trim().is_empty()
        || config.wake_phrase.chars().count() > 80
        || !config.wake_phrase.is_ascii()
    {
        return Err(anyhow!("wake phrase must contain 1-80 characters"));
    }
    if !(Duration::from_secs(2)..=Duration::from_secs(30)).contains(&config.command_window)
        || config.rolling_buffer > Duration::from_secs(10)
        || !(0.001..=0.5).contains(&config.activity_threshold)
        || config.blocked_applications.len() > 64
    {
        return Err(anyhow!("ambient voice configuration is out of bounds"));
    }
    Ok(())
}

fn emit_recognized(
    text: &str,
    ambient: Option<&AmbientVoiceConfig>,
    command_until: &mut Option<Instant>,
    events: &Sender<VoiceEvent>,
) {
    let Some(config) = ambient else {
        let _ = events.send(VoiceEvent::Final(text.to_string()));
        return;
    };
    let normalized = text.trim().to_lowercase();
    if let Some(index) = normalized.find(&config.wake_phrase.to_lowercase()) {
        let _ = events.send(VoiceEvent::WakeDetected);
        *command_until = Some(Instant::now() + config.command_window);
        let remainder = normalized[index + config.wake_phrase.len()..].trim();
        if !remainder.is_empty() {
            let _ = events.send(VoiceEvent::Command(remainder.to_string()));
            *command_until = None;
        }
    } else if command_until.is_some_and(|deadline| Instant::now() <= deadline) {
        let _ = events.send(VoiceEvent::Command(text.trim().to_string()));
        *command_until = None;
    }
}

fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean = samples
        .iter()
        .map(|sample| {
            let value = *sample as f64 / i16::MAX as f64;
            value * value
        })
        .sum::<f64>()
        / samples.len() as f64;
    mean.sqrt() as f32
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn ambient_wake_phrase_can_include_a_command() {
        let (tx, rx) = mpsc::channel();
        let config = AmbientVoiceConfig::default();
        let mut command_until = None;

        emit_recognized(
            "Hello FocalDesk run workflow morning",
            Some(&config),
            &mut command_until,
            &tx,
        );

        assert!(matches!(rx.recv().unwrap(), VoiceEvent::WakeDetected));
        assert!(matches!(
            rx.recv().unwrap(),
            VoiceEvent::Command(command) if command == "run workflow morning"
        ));
        assert!(command_until.is_none());
    }

    #[test]
    fn ambient_wake_phrase_opens_a_follow_up_window() {
        let (tx, rx) = mpsc::channel();
        let config = AmbientVoiceConfig::default();
        let mut command_until = None;

        emit_recognized("hello focaldesk", Some(&config), &mut command_until, &tx);
        assert!(matches!(rx.recv().unwrap(), VoiceEvent::WakeDetected));
        assert!(command_until.is_some());

        emit_recognized("open my inbox", Some(&config), &mut command_until, &tx);
        assert!(matches!(
            rx.recv().unwrap(),
            VoiceEvent::Command(command) if command == "open my inbox"
        ));
        assert!(command_until.is_none());
    }

    #[test]
    fn ambient_config_rejects_non_ascii_wake_phrase() {
        let config = AmbientVoiceConfig {
            wake_phrase: "héllo focaldesk".into(),
            ..AmbientVoiceConfig::default()
        };
        assert!(validate_ambient_config(&config).is_err());
    }

    #[test]
    fn rms_detects_silence_and_signal() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0, 0]), 0.0);
        assert!(rms(&[i16::MAX, i16::MAX]) > 0.99);
    }
}
