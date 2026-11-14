//! Settings owns the audio operation lifecycle; shared Rust controls own devices.
use focaldesk_audio::{
    controls::{self, AudioAction, AudioSnapshot, DeviceKind},
    meter::{InputMeter, MeterSnapshot},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

pub struct SoundService {
    operation: Mutex<()>,
    meter: Mutex<InputMeter>,
    focused: AtomicBool,
    speaker: Mutex<Option<(crate::control_process::LiveProcess, tempfile::NamedTempFile)>>,
}
impl Default for SoundService {
    fn default() -> Self {
        Self {
            operation: Mutex::new(()),
            meter: Mutex::new(InputMeter::default()),
            focused: AtomicBool::new(true),
            speaker: Mutex::new(None),
        }
    }
}
impl SoundService {
    pub fn start_speaker(&self, device_id: String) -> Result<(), String> {
        use std::io::Write;
        let _operation = self
            .operation
            .lock()
            .map_err(|_| "Audio controls unavailable.")?;
        if !self.focused.load(Ordering::Acquire) {
            return Err("Return to Settings before testing speakers.".into());
        }
        let snapshot = controls::snapshot()?;
        let device = controls::resolve_device(&snapshot, &device_id)?;
        if device.kind != DeviceKind::Output {
            return Err("Choose an output device.".into());
        }
        if device.muted {
            return Err("Unmute this output before testing it.".into());
        }
        let mut speaker = self
            .speaker
            .lock()
            .map_err(|_| "Speaker test unavailable.")?;
        *speaker = None;
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        file.write_all(&speaker_wave()).map_err(|e| e.to_string())?;
        let path = file
            .path()
            .to_str()
            .ok_or("Invalid temporary audio path.")?;
        if !self.focused.load(Ordering::Acquire) {
            return Err("Settings lost focus before the speaker test started.".into());
        }
        let process = crate::control_process::LiveProcess::start_args(
            "paplay",
            &["--device", &device.name, path],
            std::time::Duration::from_secs(4),
        )?;
        *speaker = Some((process, file));
        Ok(())
    }
    pub fn speaker_status(&self) -> Result<bool, String> {
        let mut speaker = self
            .speaker
            .lock()
            .map_err(|_| "Speaker test unavailable.")?;
        if let Some((process, _)) = speaker.as_ref() {
            let state = process.state();
            if !state.done {
                return Ok(true);
            }
            let error = state.error;
            *speaker = None;
            if let Some(error) = error {
                return Err(error);
            }
        }
        Ok(false)
    }
    pub fn stop_speaker(&self) {
        if let Ok(mut speaker) = self.speaker.lock() {
            *speaker = None;
        }
    }
    pub fn snapshot(&self) -> Result<AudioSnapshot, String> {
        controls::snapshot()
    }
    pub fn apply(&self, action: AudioAction) -> Result<AudioSnapshot, String> {
        self.stop_speaker();
        let _operation = self
            .operation
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        match &action {
            AudioAction::SetDefault { device_id }
            | AudioAction::SetMute {
                device_id,
                muted: true,
            } => {
                let snapshot = controls::snapshot()?;
                if controls::resolve_device(&snapshot, device_id)?.kind == DeviceKind::Input {
                    self.stop_all();
                }
            }
            _ => {}
        }
        controls::apply(action)
    }
    pub fn start_meter(&self, device_id: String) -> Result<MeterSnapshot, String> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        let snapshot = controls::snapshot()?;
        let device = controls::resolve_device(&snapshot, &device_id)?;
        let mut meter = self
            .meter
            .lock()
            .map_err(|_| "Microphone tests are unavailable.")?;
        if !self.focused.load(Ordering::Acquire) {
            return Err("Return to the Settings window before starting a microphone test.".into());
        }
        meter.start(device)
    }
    pub fn meter_status(&self, id: u64) -> Result<MeterSnapshot, String> {
        self.meter
            .lock()
            .map_err(|_| "Microphone tests are unavailable.")?
            .status(id)
    }
    pub fn stop_meter(&self, id: u64) -> Result<(), String> {
        self.meter
            .lock()
            .map_err(|_| "Microphone tests are unavailable.")?
            .stop(id)
    }
    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Release);
        if !focused {
            self.stop_all();
        }
    }
    pub fn stop_all(&self) {
        self.stop_speaker();
        if let Ok(mut meter) = self.meter.lock() {
            meter.stop_all();
        }
    }
}

// Short, quiet stereo tone: no downloaded assets and no shell invocation.
fn speaker_wave() -> Vec<u8> {
    let frames = 44_100_u32;
    let bytes = frames * 4;
    let mut wave = Vec::with_capacity(bytes as usize + 44);
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(bytes + 36).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16_u32.to_le_bytes());
    wave.extend_from_slice(&1_u16.to_le_bytes());
    wave.extend_from_slice(&2_u16.to_le_bytes());
    wave.extend_from_slice(&44_100_u32.to_le_bytes());
    wave.extend_from_slice(&176_400_u32.to_le_bytes());
    wave.extend_from_slice(&4_u16.to_le_bytes());
    wave.extend_from_slice(&16_u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&bytes.to_le_bytes());
    for frame in 0..frames {
        let envelope = (frame as f32 / 441.0)
            .min(1.0)
            .min((frames - frame) as f32 / 441.0);
        let sample = ((frame as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin()
            * 3276.0
            * envelope) as i16;
        wave.extend_from_slice(&sample.to_le_bytes());
        wave.extend_from_slice(&sample.to_le_bytes());
    }
    wave
}
#[cfg(test)]
mod tests {
    #[test]
    fn speaker_tone_is_bounded_stereo_pcm_with_soft_edges() {
        let wave = super::speaker_wave();
        assert_eq!(wave.len(), 176_444);
        assert_eq!(&wave[..4], b"RIFF");
        assert_eq!(&wave[44..48], &[0, 0, 0, 0]);
        assert!(wave[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|v| i16::from_le_bytes([v[0], v[1]]).unsigned_abs() <= 3276));
    }
}
