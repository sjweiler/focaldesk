pub mod sounds;

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

pub use sounds::{SAMPLE_RATE, UiSound, generate_ui_sound};

#[derive(Clone)]
pub struct SoundBuffer {
    pub sample_rate: u32,
    pub channels: u16,
    samples: Arc<[f32]>,
}

impl SoundBuffer {
    pub fn new(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            samples: samples.into(),
        }
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

pub struct UiSoundBuffers {
    hover: SoundBuffer,
    select: SoundBuffer,
    open_folder: SoundBuffer,
    close_folder: SoundBuffer,
    error: SoundBuffer,
    success: SoundBuffer,
}

impl UiSoundBuffers {
    pub fn generate() -> Self {
        Self {
            hover: generate_buffer(UiSound::Hover),
            select: generate_buffer(UiSound::Select),
            open_folder: generate_buffer(UiSound::OpenFolder),
            close_folder: generate_buffer(UiSound::CloseFolder),
            error: generate_buffer(UiSound::Error),
            success: generate_buffer(UiSound::Success),
        }
    }

    pub fn get(&self, sound: UiSound) -> &SoundBuffer {
        match sound {
            UiSound::Hover => &self.hover,
            UiSound::Select => &self.select,
            UiSound::OpenFolder => &self.open_folder,
            UiSound::CloseFolder => &self.close_folder,
            UiSound::Error => &self.error,
            UiSound::Success => &self.success,
        }
    }
}

impl Default for UiSoundBuffers {
    fn default() -> Self {
        Self::generate()
    }
}

#[derive(Default)]
pub struct UiSoundPlayer;

impl UiSoundPlayer {
    pub fn new() -> Self {
        Self
    }

    pub fn play(&self, buffer: &SoundBuffer) {
        let buffer = buffer.clone();
        thread::spawn(move || {
            let mut child = match Command::new("pw-cat")
                .args([
                    "--playback",
                    "--raw",
                    "--format",
                    "f32",
                    "--rate",
                    &buffer.sample_rate.to_string(),
                    "--channels",
                    &buffer.channels.to_string(),
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => return,
            };

            if let Some(mut stdin) = child.stdin.take() {
                let mut bytes = Vec::with_capacity(buffer.samples().len() * 4);
                for sample in buffer.samples() {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                let _ = stdin.write_all(&bytes);
            }

            let _ = child.wait();
        });
    }
}

fn generate_buffer(sound: UiSound) -> SoundBuffer {
    SoundBuffer::new(SAMPLE_RATE, 1, generate_ui_sound(sound))
}
