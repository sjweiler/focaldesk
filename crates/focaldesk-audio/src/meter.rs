//! Explicit, bounded microphone tests. Raw samples are reduced to levels in
//! Rust, never sent to the webview, transcribed, or written to a recording.
use crate::controls::{AudioDevice, DeviceKind};
use serde::Serialize;
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const TEST_DURATION: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
pub struct MeterSnapshot {
    pub id: u64,
    pub device_id: String,
    pub active: bool,
    pub peak: f32,
    pub rms: f32,
    pub remaining_ms: u64,
    pub error: Option<String>,
}

struct Session {
    state: Arc<Mutex<MeterSnapshot>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
pub struct InputMeter {
    session: Option<Session>,
    sequence: u64,
}

impl InputMeter {
    pub fn start(&mut self, device: &AudioDevice) -> Result<MeterSnapshot, String> {
        if device.kind != DeviceKind::Input {
            return Err("Choose an input device for the microphone test.".into());
        }
        if device.muted {
            return Err("Unmute this microphone before starting the level test.".into());
        }
        self.session = None;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Microphone test sequence exhausted.")?;
        let mut command = Command::new("parec");
        command
            .args([
                "--raw",
                "--property=node.dont-reconnect=true",
                "--property=stream.dont-reconnect=true",
                "--format=float32le",
                "--rate=16000",
                "--channels=1",
                "--latency-msec=50",
                "--client-name=FocalDesk Settings",
                "--stream-name=Microphone level test",
                &format!("--device={}", device.name),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // If Settings crashes, the audio helper must not survive it. Also check
        // the parent after arming the signal to cover the fork/prctl race.
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: the child hook uses only async-signal-safe system calls and
        // does not allocate or access locks before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("Microphone level tests require parec: {error}"))?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Microphone stream is unavailable.".into());
        };
        // Nonblocking reads let the worker honor Stop and its deadline even if
        // a disconnected device stops producing samples.
        // SAFETY: fcntl operates on the live pipe owned by stdout.
        let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Could not configure the microphone test stream.".into());
        }
        let initial = MeterSnapshot {
            id: self.sequence,
            device_id: device.id.clone(),
            active: true,
            peak: 0.0,
            rms: 0.0,
            remaining_ms: TEST_DURATION.as_millis() as u64,
            error: None,
        };
        let state = Arc::new(Mutex::new(initial.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_stop = stop.clone();
        let deadline = Instant::now() + TEST_DURATION;
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            let mut tail = Vec::with_capacity(4);
            let mut last_samples = Instant::now();
            while !worker_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                let read = stdout.read(&mut buffer);
                let mut status = worker_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                status.remaining_ms = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                match read {
                    Ok(0) => {
                        status.error = Some("The microphone stream ended. Check that the device is still connected.".into());
                        break;
                    }
                    Ok(count) => {
                        last_samples = Instant::now();
                        tail.extend_from_slice(&buffer[..count]);
                        let complete = tail.len() / 4 * 4;
                        (status.peak, status.rms) = pcm_levels(&tail[..complete]);
                        tail.drain(..complete);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if last_samples.elapsed() > Duration::from_millis(250) {
                            status.peak = 0.0;
                            status.rms = 0.0;
                        }
                    }
                    Err(error) => {
                        status.error = Some(format!("Microphone test failed: {error}"));
                        break;
                    }
                }
                drop(status);
                thread::sleep(Duration::from_millis(15));
            }
            let _ = child.kill();
            let _ = child.wait();
            let mut status = worker_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.active = false;
            status.remaining_ms = 0;
            status.peak = 0.0;
            status.rms = 0.0;
        });
        self.session = Some(Session {
            state,
            stop,
            worker: Some(worker),
        });
        Ok(initial)
    }

    pub fn status(&self, id: u64) -> Result<MeterSnapshot, String> {
        let session = self
            .session
            .as_ref()
            .ok_or("No microphone test is active.")?;
        let status = session
            .state
            .lock()
            .map_err(|_| "Microphone test state is unavailable.")?
            .clone();
        if status.id != id {
            return Err("This microphone test is no longer current.".into());
        }
        Ok(status)
    }

    pub fn stop(&mut self, id: u64) -> Result<(), String> {
        self.status(id)?;
        self.session = None;
        Ok(())
    }
    pub fn stop_all(&mut self) {
        self.session = None;
    }
}

fn pcm_levels(bytes: &[u8]) -> (f32, f32) {
    let (samples, _) = bytes.as_chunks::<4>();
    let mut peak = 0.0_f32;
    let mut sum = 0.0_f64;
    let mut count = 0_u32;
    for bytes in samples {
        let value = f32::from_le_bytes(*bytes);
        if !value.is_finite() {
            continue;
        }
        let value = value.clamp(-1.0, 1.0);
        peak = peak.max(value.abs());
        sum += f64::from(value).powi(2);
        count += 1;
    }
    (
        peak,
        if count > 0 {
            (sum / f64::from(count)).sqrt() as f32
        } else {
            0.0
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meters_float_pcm_without_exposing_samples_and_handles_partial_invalid_data() {
        let data = [0.5_f32, -0.5, f32::NAN, f32::INFINITY]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(pcm_levels(&data), (0.5, 0.5));
        assert_eq!(pcm_levels(&[0, 0, 0]), (0.0, 0.0));
        let clipped = [2.0_f32, -2.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(pcm_levels(&clipped), (1.0, 1.0));
    }
}
