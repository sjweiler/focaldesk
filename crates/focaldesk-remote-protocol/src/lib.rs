//! Versioned local transport used between the compositor and remote-desktop service.

use std::io::{self, IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use nix::cmsg_space;
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    StartCapture { output_id: u64 },
    StopCapture { session_id: u64 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgba8888,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputTransform {
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    CaptureStarted {
        session_id: u64,
        output_id: u64,
        width: u32,
        height: u32,
        scale: f64,
        transform: OutputTransform,
    },
    FrameReady {
        session_id: u64,
        frame_serial: u64,
        width: u32,
        height: u32,
        stride: u32,
        len: u64,
        format: PixelFormat,
        scale: f64,
        transform: OutputTransform,
        full_refresh: bool,
    },
    CaptureStopped {
        session_id: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u16,
    payload: T,
}

pub fn write_message<T: Serialize>(stream: &UnixStream, payload: &T) -> io::Result<()> {
    let body = serde_json::to_vec(&Envelope {
        version: PROTOCOL_VERSION,
        payload,
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote IPC message is too large",
        ));
    }
    send_all(stream, &(body.len() as u32).to_le_bytes())?;
    send_all(stream, &body)
}

pub fn read_message<T: DeserializeOwned>(stream: &UnixStream) -> io::Result<T> {
    let mut length = [0_u8; 4];
    receive_exact(stream, &mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid remote IPC message length",
        ));
    }
    let mut body = vec![0_u8; length];
    receive_exact(stream, &mut body)?;
    let envelope: Envelope<T> = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported remote protocol version {}", envelope.version),
        ));
    }
    Ok(envelope.payload)
}

fn send_all(stream: &UnixStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let iov = [IoSlice::new(bytes)];
        match sendmsg::<()>(stream.as_raw_fd(), &iov, &[], MsgFlags::MSG_NOSIGNAL, None) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
            Ok(written) => bytes = &bytes[written..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
    Ok(())
}

fn receive_exact(stream: &UnixStream, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let received = {
            let mut iov = [IoSliceMut::new(bytes)];
            match recvmsg::<()>(stream.as_raw_fd(), &mut iov, None, MsgFlags::empty()) {
                Ok(message) if message.bytes == 0 => {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                Ok(message) => message.bytes,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        };
        bytes = &mut bytes[received..];
    }
    Ok(())
}

pub fn send_fd(stream: &UnixStream, fd: RawFd) -> io::Result<()> {
    let marker = [0x46_u8];
    let iov = [IoSlice::new(&marker)];
    sendmsg::<()>(
        stream.as_raw_fd(),
        &iov,
        &[ControlMessage::ScmRights(&[fd])],
        MsgFlags::MSG_NOSIGNAL,
        None,
    )
    .map(|_| ())
    .map_err(io::Error::other)
}

pub fn receive_fd(stream: &UnixStream) -> io::Result<OwnedFd> {
    let mut marker = [0_u8];
    let mut iov = [IoSliceMut::new(&mut marker)];
    let mut control = cmsg_space!([RawFd; 1]);
    let message = recvmsg::<()>(
        stream.as_raw_fd(),
        &mut iov,
        Some(&mut control),
        MsgFlags::MSG_CMSG_CLOEXEC,
    )
    .map_err(io::Error::other)?;
    let received_bytes = message.bytes;
    let received_fd = message.cmsgs().find_map(|message| {
        if let ControlMessageOwned::ScmRights(fds) = message {
            fds.into_iter().next()
        } else {
            None
        }
    });
    let _ = message;
    if received_bytes != 1 || marker[0] != 0x46 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing shared-buffer descriptor marker",
        ));
    }
    if let Some(fd) = received_fd {
        // SAFETY: SCM_RIGHTS created a new descriptor owned by this process.
        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "frame event did not include a shared-buffer descriptor",
    ))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn request_round_trips_with_version_envelope() {
        let (server, client) = UnixStream::pair().unwrap();
        let expected = Request::StartCapture { output_id: 7 };
        write_message(&server, &expected).unwrap();
        assert_eq!(read_message::<Request>(&client).unwrap(), expected);
    }

    #[test]
    fn rejects_oversized_length_before_allocating() {
        let (server, client) = UnixStream::pair().unwrap();
        send_all(&server, &((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes()).unwrap();
        assert_eq!(
            read_message::<Request>(&client).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn rejects_truncated_messages() {
        let (server, client) = UnixStream::pair().unwrap();
        send_all(&server, &10_u32.to_le_bytes()).unwrap();
        send_all(&server, b"short").unwrap();
        drop(server);
        assert_eq!(
            read_message::<Request>(&client).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn descriptor_round_trips_with_close_on_exec() {
        let file = File::open("/dev/null").unwrap();
        let (server, client) = UnixStream::pair().unwrap();
        send_fd(&server, file.as_fd().as_raw_fd()).unwrap();
        let received = receive_fd(&client).unwrap();
        let flags = unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
