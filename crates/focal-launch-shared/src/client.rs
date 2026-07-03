use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use std::time::Instant;

use crate::{DEFAULT_TIMEOUT_MS, LaunchError, LaunchRequest, LaunchResponse, Result, socket_path};

pub fn request_launch(req: &LaunchRequest) -> Result<LaunchResponse> {
    let socket = socket_path();
    let started = Instant::now();
    eprintln!(
        "focal-launch-client: connect trace_id={} app={} source={:?} socket={}",
        req.trace_id,
        req.app,
        req.source,
        socket.display()
    );
    let mut stream = UnixStream::connect(&socket).map_err(|err| {
        if matches!(
            err.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::TimedOut
        ) {
            LaunchError::DaemonUnavailable
        } else {
            LaunchError::Io(err)
        }
    })?;
    let timeout = Some(Duration::from_millis(DEFAULT_TIMEOUT_MS));
    stream.set_write_timeout(timeout)?;
    stream.set_read_timeout(timeout)?;

    let json = serde_json::to_vec(req)?;
    stream.write_all(&json)?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    BufReader::new(stream).read_to_string(&mut response)?;
    let Some(line) = response.lines().find(|line| !line.trim().is_empty()) else {
        return Err(LaunchError::DaemonUnavailable);
    };
    let parsed = serde_json::from_str::<LaunchResponse>(line)?;
    eprintln!(
        "focal-launch-client: response trace_id={} app={} elapsed_ms={} response={:?}",
        req.trace_id,
        req.app,
        started.elapsed().as_millis(),
        parsed
    );
    match parsed {
        LaunchResponse::Accepted => Ok(LaunchResponse::Accepted),
        LaunchResponse::Failed { message } => Err(LaunchError::LaunchFailed(message)),
    }
}
