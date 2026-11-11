//! Native FocalDesk output recorder.
//!
//! This is deliberately a client of the compositor's bounded capture broker.
//! It never reads DRM framebuffers or asks applications for their contents.

use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use focaldesk_remote_protocol::{
    read_message, receive_fd, write_message, CapturePurpose, Event, Request, MAX_FRAME_BYTES,
};

const SOCKET_ENV: &str = "FOCALDESK_REMOTE_SOCKET_PATH";
const SOCKET_NAME: &str = "remote-capture.sock";
const DEFAULT_FPS: u32 = 30;
const CONTROL_SOCKET_NAME: &str = "recorder-control.sock";

#[derive(Debug)]
struct Config {
    output_id: u64,
    destination: PathBuf,
    fps: u32,
}

fn main() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--stop"] {
        return stop();
    }
    record(parse_args(args)?)
}

fn parse_args(args: Vec<String>) -> Result<Config> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        std::process::exit(0);
    }

    let mut output_id = None;
    let mut destination = None;
    let mut fps = DEFAULT_FPS;
    let mut confirmed = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output_id = Some(parse_u64("--output", args.next())?),
            "--file" => destination = Some(PathBuf::from(required_value("--file", args.next())?)),
            "--fps" => {
                fps = parse_u32("--fps", args.next())?;
                if !(1..=120).contains(&fps) {
                    bail!("--fps must be between 1 and 120");
                }
            }
            "--confirm" => confirmed = true,
            value => bail!("unknown argument {value}; run with --help"),
        }
    }
    if !confirmed {
        bail!("recording requires --confirm; screen recording is always an explicit action");
    }
    let destination = destination.context("--file is required")?;
    if destination.exists() {
        bail!(
            "refusing to overwrite existing recording {}",
            destination.display()
        );
    }
    let parent = destination
        .parent()
        .context("recording path has no parent directory")?;
    if !parent.is_dir() {
        bail!("recording directory does not exist: {}", parent.display());
    }
    Ok(Config {
        output_id: output_id.context("--output is required")?,
        destination,
        fps,
    })
}

fn required_value(flag: &str, value: Option<String>) -> Result<String> {
    value.with_context(|| format!("{flag} requires a value"))
}

fn parse_u64(flag: &str, value: Option<String>) -> Result<u64> {
    required_value(flag, value)?
        .parse()
        .with_context(|| format!("{flag} must be a positive integer"))
}

fn parse_u32(flag: &str, value: Option<String>) -> Result<u32> {
    required_value(flag, value)?
        .parse()
        .with_context(|| format!("{flag} must be a positive integer"))
}

fn record(config: Config) -> Result<()> {
    let control = bind_control_socket()?;
    control.set_nonblocking(true)?;
    let stream =
        UnixStream::connect(socket_path()?).context("connect to FocalDesk capture service")?;
    write_message(
        &stream,
        &Request::StartCapture {
            output_id: config.output_id,
            purpose: CapturePurpose::Recording,
        },
    )?;
    let started = read_message::<Event>(&stream)?;
    let Event::CaptureStarted {
        session_id,
        width,
        height,
        ..
    } = started
    else {
        bail!("capture did not start: {}", event_error(&started));
    };
    if width == 0 || height == 0 {
        bail!("selected output has no usable size");
    }

    let mut encoder = Encoder::start(&config.destination, width, height, config.fps)?;
    eprintln!(
        "Recording output {} to {}. Press Ctrl-C to stop.",
        config.output_id,
        config.destination.display()
    );

    let result = receive_frames(&stream, session_id, width, height, &mut encoder, &control);
    let _ = write_message(&stream, &Request::StopCapture { session_id });
    encoder.finish()?;
    result
}

fn receive_frames(
    stream: &UnixStream,
    session_id: u64,
    width: u32,
    height: u32,
    encoder: &mut Encoder,
    control: &UnixListener,
) -> Result<()> {
    let expected_len = usize::try_from(u64::from(width) * u64::from(height) * 4)
        .context("output is too large to record")?;
    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(encoder.fps));
    let mut next_frame_at = Instant::now();
    let mut latest = vec![0_u8; expected_len];
    let mut have_frame = false;
    let mut timer_enabled = false;

    loop {
        if let Ok((mut client, _)) = control.accept() {
            let mut command = String::new();
            client.read_to_string(&mut command)?;
            if command.trim() == "stop" {
                client.write_all(b"stopping\n")?;
                return Ok(());
            }
        }
        let event = match read_message::<Event>(stream) {
            Ok(event) => event,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && have_frame =>
            {
                encoder.write_frame(&latest)?;
                next_frame_at += frame_interval;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        match event {
            Event::FrameReady {
                session_id: received,
                width: frame_width,
                height: frame_height,
                stride,
                len,
                ..
            } => {
                if received != session_id {
                    continue;
                }
                if frame_width != width
                    || frame_height != height
                    || stride != width.saturating_mul(4)
                    || len as usize != expected_len
                    || len > MAX_FRAME_BYTES
                {
                    bail!("capture geometry changed; stop and start a new recording");
                }
                let fd = receive_fd(stream)?;
                let mut frame = File::from(fd);
                frame
                    .read_exact(&mut latest)
                    .context("read captured frame")?;
                if !timer_enabled {
                    stream.set_read_timeout(Some(frame_interval))?;
                    timer_enabled = true;
                    next_frame_at = Instant::now();
                }
                have_frame = true;

                // The broker only sends damage. Repeat the latest full frame to keep
                // a constant timeline while the desktop is idle.
                let now = Instant::now();
                while next_frame_at <= now {
                    encoder.write_frame(&latest)?;
                    next_frame_at += frame_interval;
                }
            }
            Event::CaptureStopped {
                session_id: received,
            } if received == session_id => return Ok(()),
            Event::Error { message } => bail!("capture failed: {message}"),
            _ => {}
        }
    }
}

struct Encoder {
    child: Child,
    stdin: ChildStdin,
    fps: u32,
}

impl Encoder {
    fn start(destination: &Path, width: u32, height: u32, fps: u32) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "warning",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgba",
                "-video_size",
                &format!("{width}x{height}"),
                "-framerate",
                &fps.to_string(),
                "-i",
                "-",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
                "-n",
            ])
            .arg(destination)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("start ffmpeg (install it to enable native recording)")?;
        let stdin = child
            .stdin
            .take()
            .context("ffmpeg did not provide an input pipe")?;
        Ok(Self { child, stdin, fps })
    }

    fn write_frame(&mut self, pixels: &[u8]) -> Result<()> {
        self.stdin
            .write_all(pixels)
            .context("send frame to video encoder")
    }

    fn finish(mut self) -> Result<()> {
        drop(self.stdin);
        let status = self.child.wait().context("wait for video encoder")?;
        if !status.success() {
            bail!("ffmpeg failed with {status}");
        }
        Ok(())
    }
}

fn socket_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let runtime = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("focaldesk").join(SOCKET_NAME))
}

fn control_socket_path() -> Result<PathBuf> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime)
        .join("focaldesk")
        .join(CONTROL_SOCKET_NAME))
}

fn bind_control_socket() -> Result<UnixListener> {
    let path = control_socket_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    focaldesk_ipc::transport::bind_user_socket(&path).context("bind recorder control socket")
}

fn stop() -> Result<()> {
    let mut stream = UnixStream::connect(control_socket_path()?).context("no active recording")?;
    stream.write_all(b"stop\n")?;
    println!("Stopping recording; finalizing video file.");
    Ok(())
}

fn event_error(event: &Event) -> String {
    match event {
        Event::Error { message } => message.clone(),
        _ => "unexpected compositor response".into(),
    }
}

fn print_help() {
    println!("Usage: focaldesk-recorder --output ID --file PATH [--fps FPS] --confirm");
    println!("       focaldesk-recorder --stop");
    println!("\nRecord one FocalDesk output as an SDR H.264 MP4 without OBS.");
    println!("Use focaldesk-cli desktop-snapshot to find output IDs.");
    println!(
        "Recording is explicit: --confirm is required and existing files are never overwritten."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_explicit_confirmation() {
        assert!(parse_args(vec![
            "--output".into(),
            "1".into(),
            "--file".into(),
            "/tmp/new.mp4".into()
        ])
        .is_err());
    }

    #[test]
    fn rejects_invalid_fps() {
        assert!(parse_args(vec![
            "--output".into(),
            "1".into(),
            "--file".into(),
            "/tmp/new.mp4".into(),
            "--fps".into(),
            "121".into(),
            "--confirm".into()
        ])
        .is_err());
    }
}
