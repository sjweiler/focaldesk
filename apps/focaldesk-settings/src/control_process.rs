//! Bounded CLI helpers for fixed Rust-owned control operations.
use std::os::unix::process::CommandExt;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1");
    // Helpers must not survive a Settings crash. Check the fork/exec parent race.
    let parent = std::process::id();
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent as libc::pid_t {
                return Err(std::io::Error::other("Settings exited"));
            }
            Ok(())
        });
    }
    command
}

fn drain(stream: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.take(1_048_577).read_to_end(&mut bytes);
        bytes
    })
}

pub(crate) fn run(
    program: &str,
    args: &[&str],
    input: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| format!("{program} is unavailable."))?;
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = input {
            let _ = writeln!(stdin, "{input}");
        }
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{program} timed out or stopped unexpectedly."));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    let status = status?;
    if stdout.len() > 1_048_576 || stderr.len() > 1_048_576 {
        return Err(format!("{program} returned too much data."));
    }
    if !status.success() {
        // Secret prompts can echo input. Never send those transcripts to the webview.
        if input.is_some() {
            return Err("Connection failed. Check the password and try again.".into());
        }
        return Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

#[derive(Clone, Default)]
pub(crate) struct ProcessState {
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
}
pub(crate) struct LiveProcess {
    state: Arc<Mutex<ProcessState>>,
    cancel: Arc<AtomicBool>,
    input: mpsc::Sender<String>,
    worker: Option<thread::JoinHandle<()>>,
}
impl LiveProcess {
    pub fn start(program: &str, timeout: Duration) -> Result<Self, String> {
        Self::start_args(program, &[], timeout)
    }
    pub fn start_args(program: &str, args: &[&str], timeout: Duration) -> Result<Self, String> {
        let mut child = command(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| format!("{program} is unavailable."))?;
        let mut stdin = child.stdin.take().unwrap();
        let state = Arc::new(Mutex::new(ProcessState::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (output_tx, output_rx) = mpsc::sync_channel(64);
        for mut stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = output_tx.clone();
            thread::spawn(move || {
                let mut buffer = [0; 512];
                while let Ok(count) = stream.read(&mut buffer) {
                    if count == 0
                        || tx
                            .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                            .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(output_tx);
        let (input, input_rx) = mpsc::channel::<String>();
        let shared = state.clone();
        let cancelled = cancel.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            loop {
                for chunk in output_rx.try_iter().take(64) {
                    let mut state = shared.lock().unwrap();
                    state.text.push_str(&chunk);
                    if state.text.len() > 65_536 {
                        state.error = Some("Control helper returned too much data.".into());
                        cancelled.store(true, Ordering::Release);
                    }
                }
                if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut state = shared.lock().unwrap();
                    state.done = true;
                    if !cancelled.load(Ordering::Acquire) {
                        state.error = Some("Control operation timed out.".into());
                    }
                    break;
                }
                for line in input_rx.try_iter() {
                    if writeln!(stdin, "{line}")
                        .and_then(|_| stdin.flush())
                        .is_err()
                    {
                        cancelled.store(true, Ordering::Release);
                    }
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let mut state = shared.lock().unwrap();
                        state.done = true;
                        if !status.success() {
                            state.error = Some("Control helper stopped unexpectedly.".into());
                        }
                        break;
                    }
                    Err(_) => {
                        cancelled.store(true, Ordering::Release);
                    }
                    _ => {}
                }
                thread::sleep(Duration::from_millis(30));
            }
        });
        Ok(Self {
            state,
            cancel,
            input,
            worker: Some(worker),
        })
    }
    pub fn send(&self, value: &str) -> Result<(), String> {
        if value.len() > 512 || value.contains(['\n', '\r', '\0']) {
            return Err("Invalid Bluetooth response.".into());
        }
        self.input
            .send(value.into())
            .map_err(|_| "Bluetooth operation already ended.".into())
    }
    pub fn state(&self) -> ProcessState {
        self.state.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.state.lock().unwrap().text.clear();
    }
}
impl Drop for LiveProcess {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_stops_a_helper_without_frontend_polling() {
        let process = LiveProcess::start("cat", Duration::from_millis(150)).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(process.state().done);
        assert!(process.state().error.unwrap().contains("timed out"));
    }
    #[test]
    fn bounded_commands_time_out_and_secret_errors_are_redacted() {
        let before = Instant::now();
        assert!(run("sleep", &["5"], None, Duration::from_millis(100))
            .unwrap_err()
            .contains("timed out"));
        assert!(before.elapsed() < Duration::from_secs(2));
        let error = run(
            "sh",
            &["-c", "read value; echo \"$value\"; exit 1"],
            Some("private-test-token"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(!error.contains("private-test-token"));
        assert!(error.contains("Check the password"));
    }
}
