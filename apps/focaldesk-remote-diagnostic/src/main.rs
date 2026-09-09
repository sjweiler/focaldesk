use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use focaldesk_remote_protocol::{read_message, receive_fd, write_message, Event, Request};

fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FOCALDESK_REMOTE_SOCKET_PATH") {
        return Ok(path.into());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("focaldesk/remote-capture.sock"))
}

fn main() -> Result<()> {
    let output_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1".into())
        .parse::<u64>()
        .context("output id must be an integer")?;
    let destination = std::env::args().nth(2).map(PathBuf::from);
    let stream = UnixStream::connect(socket_path()?).context("connect to compositor")?;
    write_message(&stream, &Request::StartCapture { output_id })?;
    let mut active_session = None;

    loop {
        match read_message::<Event>(&stream)? {
            Event::CaptureStarted {
                session_id,
                width,
                height,
                ..
            } => {
                active_session = Some(session_id);
                eprintln!("capture session {session_id} started at {width}x{height}");
            }
            Event::FrameReady {
                len,
                frame_serial,
                height,
                stride,
                ..
            } => {
                let fd = receive_fd(&stream)?;
                if len > focaldesk_remote_protocol::MAX_FRAME_BYTES {
                    bail!("compositor advertised an oversized frame");
                }
                if len != u64::from(stride) * u64::from(height) {
                    bail!("compositor advertised inconsistent frame dimensions");
                }
                let mut pixels = vec![0_u8; len as usize];
                let mut file = File::from(fd);
                file.read_exact(&mut pixels)?;
                if let Some(path) = destination.as_ref() {
                    File::create(path)?.write_all(&pixels)?;
                    println!("wrote frame {frame_serial} to {}", path.display());
                    if let Some(session_id) = active_session {
                        write_message(&stream, &Request::StopCapture { session_id })?;
                    }
                    return Ok(());
                }
                println!("received frame {frame_serial} ({len} bytes)");
            }
            Event::Error { message } => bail!(message),
            Event::CaptureStopped { .. } => return Ok(()),
        }
    }
}
