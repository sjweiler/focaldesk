//! Local-only compositor transport for future network-facing remote desktop services.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use focaldesk_remote_protocol::{
    send_fd, write_message, Event, OutputTransform, PixelFormat, Request,
};
use focaldesk_types::OutputId;
use smithay::backend::renderer::gles::GlesRenderer;

use crate::core::capture::CaptureConsumerId;
use crate::core::desktop::{DamageSource, DesktopState};

const SOCKET_ENV: &str = "FOCALDESK_REMOTE_SOCKET_PATH";
const SOCKET_NAME: &str = "remote-capture.sock";
const OUTBOUND_QUEUE_DEPTH: usize = 2;

enum RemoteCommand {
    Start {
        output_id: OutputId,
        outbound: SyncSender<RemoteOutbound>,
    },
    Disconnected {
        session_id: u64,
    },
    Stop {
        session_id: u64,
    },
}

struct RemoteOutbound {
    event: Event,
    fd: Option<OwnedFd>,
}

struct RemoteSession {
    consumer_id: CaptureConsumerId,
    outbound: SyncSender<RemoteOutbound>,
}

pub struct LocalRemoteCapture {
    commands: Receiver<RemoteCommand>,
    sessions: HashMap<u64, RemoteSession>,
    next_session_id: u64,
}

impl LocalRemoteCapture {
    pub fn start() -> io::Result<Self> {
        let path = socket_path()?;
        let listener = focaldesk_ipc::transport::bind_user_socket(&path)?;
        let (commands_tx, commands) = mpsc::channel();
        thread::Builder::new()
            .name("focaldesk-remote-local".into())
            .spawn(move || accept_loop(listener, commands_tx))?;
        focaldesk_logging::flog(format!(
            "local remote capture transport listening at {}",
            path.display()
        ));
        Ok(Self {
            commands,
            sessions: HashMap::new(),
            next_session_id: 1,
        })
    }

    fn drain_commands(&mut self, state: &mut DesktopState) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                RemoteCommand::Start {
                    output_id,
                    outbound,
                } => {
                    let Some(output) = state.outputs.get(&output_id) else {
                        let _ = outbound.try_send(RemoteOutbound {
                            event: Event::Error {
                                message: format!("unknown output {}", output_id.0),
                            },
                            fd: None,
                        });
                        continue;
                    };
                    let width = output.physical_size.w.max(0) as u32;
                    let height = output.physical_size.h.max(0) as u32;
                    let scale = output.scale_factor;
                    let transform = protocol_transform(output.handle.current_transform());
                    let session_id = self.next_session_id;
                    self.next_session_id = self.next_session_id.saturating_add(1);
                    let consumer_id = state.output_capture_broker.register(output_id, 2);
                    self.sessions.insert(
                        session_id,
                        RemoteSession {
                            consumer_id,
                            outbound: outbound.clone(),
                        },
                    );
                    let _ = outbound.try_send(RemoteOutbound {
                        event: Event::CaptureStarted {
                            session_id,
                            output_id: output_id.0,
                            width,
                            height,
                            scale,
                            transform,
                        },
                        fd: None,
                    });
                    state.mark_output_full_damage(output_id, DamageSource::Unknown);
                }
                RemoteCommand::Disconnected { session_id } => {
                    if let Some(session) = self.sessions.remove(&session_id) {
                        state.output_capture_broker.remove(session.consumer_id);
                    }
                }
                RemoteCommand::Stop { session_id } => {
                    if let Some(session) = self.sessions.remove(&session_id) {
                        state.output_capture_broker.remove(session.consumer_id);
                        let _ = session.outbound.try_send(RemoteOutbound {
                            event: Event::CaptureStopped { session_id },
                            fd: None,
                        });
                    }
                }
            }
        }
    }

    fn export_frames(&mut self, state: &mut DesktopState, renderer: &mut GlesRenderer) {
        let session_ids = self.sessions.keys().copied().collect::<Vec<_>>();
        let mut disconnected = Vec::new();
        for session_id in session_ids {
            let Some(session) = self.sessions.get(&session_id) else {
                continue;
            };
            let consumer_id = session.consumer_id;
            let outbound = session.outbound.clone();
            let Some(frame) = state.output_capture_broker.take_latest_frame(consumer_id) else {
                continue;
            };
            let pixels = match crate::core::portal::read_capture_source_rgba(
                state,
                renderer,
                frame.output_id,
                &frame.buffer,
            ) {
                Ok(pixels) => pixels,
                Err(error) => {
                    focaldesk_logging::flog(format!("remote frame readback failed: {error}"));
                    continue;
                }
            };
            let Ok(fd) = sealed_memfd(&pixels) else {
                continue;
            };
            let width = frame.geometry.size.w.max(0) as u32;
            let height = frame.geometry.size.h.max(0) as u32;
            let message = RemoteOutbound {
                event: Event::FrameReady {
                    session_id,
                    frame_serial: frame.serial,
                    width,
                    height,
                    stride: width.saturating_mul(4),
                    len: pixels.len() as u64,
                    format: PixelFormat::Rgba8888,
                    scale: frame.geometry.scale,
                    transform: protocol_transform(frame.geometry.transform),
                    full_refresh: frame.full_refresh,
                },
                fd: Some(fd),
            };
            match outbound.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => disconnected.push(session_id),
            }
        }
        for session_id in disconnected {
            if let Some(session) = self.sessions.remove(&session_id) {
                state.output_capture_broker.remove(session.consumer_id);
            }
        }
    }
}

pub fn process_commands(state: &mut DesktopState) {
    let Some(mut remote) = state.local_remote_capture.take() else {
        return;
    };
    remote.drain_commands(state);
    state.local_remote_capture = Some(remote);
}

pub fn export_frames(state: &mut DesktopState, renderer: &mut GlesRenderer) {
    let Some(mut remote) = state.local_remote_capture.take() else {
        return;
    };
    remote.export_frames(state, renderer);
    state.local_remote_capture = Some(remote);
}

fn socket_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(PathBuf::from(runtime).join("focaldesk").join(SOCKET_NAME))
}

fn protocol_transform(transform: smithay::utils::Transform) -> OutputTransform {
    match transform {
        smithay::utils::Transform::Normal => OutputTransform::Normal,
        smithay::utils::Transform::_90 => OutputTransform::Rotate90,
        smithay::utils::Transform::_180 => OutputTransform::Rotate180,
        smithay::utils::Transform::_270 => OutputTransform::Rotate270,
        smithay::utils::Transform::Flipped => OutputTransform::Flipped,
        smithay::utils::Transform::Flipped90 => OutputTransform::Flipped90,
        smithay::utils::Transform::Flipped180 => OutputTransform::Flipped180,
        smithay::utils::Transform::Flipped270 => OutputTransform::Flipped270,
    }
}

fn accept_loop(listener: UnixListener, commands: mpsc::Sender<RemoteCommand>) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        if focaldesk_ipc::transport::require_authorized_peer(
            &stream,
            focaldesk_ipc::transport::REMOTE_CAPTURE_POLICY,
        )
        .is_err()
        {
            continue;
        }
        // Phase 2 deliberately serves one diagnostic client at a time. Keeping
        // acceptance serial also bounds client threads and compositor resources.
        handle_client(stream, commands.clone());
    }
}

fn handle_client(mut stream: UnixStream, commands: mpsc::Sender<RemoteCommand>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = match focaldesk_remote_protocol::read_message::<Request>(&mut stream) {
        Ok(request) => request,
        Err(_) => return,
    };
    let Request::StartCapture { output_id } = request else {
        let _ = write_message(
            &mut stream,
            &Event::Error {
                message: "first request must start capture".into(),
            },
        );
        return;
    };
    let (outbound, incoming) = mpsc::sync_channel(OUTBOUND_QUEUE_DEPTH);
    if commands
        .send(RemoteCommand::Start {
            output_id: OutputId(output_id),
            outbound,
        })
        .is_err()
    {
        return;
    }
    let Ok(first) = incoming.recv() else {
        return;
    };
    let Event::CaptureStarted { session_id, .. } = first.event else {
        let _ = write_message(&mut stream, &first.event);
        return;
    };
    if write_message(&mut stream, &first.event).is_err() {
        let _ = commands.send(RemoteCommand::Disconnected { session_id });
        return;
    }

    let _ = stream.set_read_timeout(None);
    if let Ok(mut reader) = stream.try_clone() {
        let reader_commands = commands.clone();
        let _ = thread::Builder::new()
            .name("focaldesk-remote-reader".into())
            .spawn(move || loop {
                match focaldesk_remote_protocol::read_message::<Request>(&mut reader) {
                    Ok(Request::StopCapture {
                        session_id: requested,
                    }) if requested == session_id => {
                        let _ = reader_commands.send(RemoteCommand::Stop { session_id });
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => {
                        let _ = reader_commands.send(RemoteCommand::Disconnected { session_id });
                        return;
                    }
                }
            });
    }

    while let Ok(message) = incoming.recv() {
        if write_message(&mut stream, &message.event).is_err() {
            break;
        }
        if let Some(fd) = message.fd {
            if send_fd(&stream, fd.as_raw_fd()).is_err() {
                break;
            }
        }
        if matches!(
            message.event,
            Event::CaptureStopped { .. } | Event::Error { .. }
        ) {
            break;
        }
    }
    let _ = commands.send(RemoteCommand::Disconnected { session_id });
}

fn sealed_memfd(bytes: &[u8]) -> io::Result<OwnedFd> {
    if bytes.len() as u64 > focaldesk_remote_protocol::MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame is too large",
        ));
    }
    let name = CString::new("focaldesk-remote-frame").unwrap();
    // SAFETY: memfd_create returns a new descriptor on success.
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw is a newly owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn exported_frame_memfd_is_readable_and_immutable() {
        let expected = b"frame pixels";
        let fd = sealed_memfd(expected).unwrap();
        let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(
            seals
                & (libc::F_SEAL_SEAL
                    | libc::F_SEAL_SHRINK
                    | libc::F_SEAL_GROW
                    | libc::F_SEAL_WRITE),
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE
        );
        let mut file = File::from(fd);
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
        assert!(file.write_all(b"tamper").is_err());
    }

    #[test]
    fn local_client_can_start_and_stop_a_session() {
        let (server, client) = UnixStream::pair().unwrap();
        let (commands_tx, commands_rx) = mpsc::channel();
        let worker = thread::spawn(move || handle_client(server, commands_tx));

        focaldesk_remote_protocol::write_message(&client, &Request::StartCapture { output_id: 9 })
            .unwrap();
        let (output_id, outbound) = match commands_rx.recv().unwrap() {
            RemoteCommand::Start {
                output_id,
                outbound,
            } => (output_id, outbound),
            _ => panic!("expected start command"),
        };
        assert_eq!(output_id, OutputId(9));
        outbound
            .send(RemoteOutbound {
                event: Event::CaptureStarted {
                    session_id: 42,
                    output_id: 9,
                    width: 1920,
                    height: 1080,
                    scale: 1.0,
                    transform: OutputTransform::Normal,
                },
                fd: None,
            })
            .unwrap();
        assert!(matches!(
            focaldesk_remote_protocol::read_message::<Event>(&client).unwrap(),
            Event::CaptureStarted { session_id: 42, .. }
        ));

        focaldesk_remote_protocol::write_message(&client, &Request::StopCapture { session_id: 42 })
            .unwrap();
        assert!(matches!(
            commands_rx.recv().unwrap(),
            RemoteCommand::Stop { session_id: 42 }
        ));
        drop(outbound);
        drop(client);
        worker.join().unwrap();
    }
}
