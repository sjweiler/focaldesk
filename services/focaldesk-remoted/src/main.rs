use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::{NonZeroU16, NonZeroUsize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use focaldesk_remote_protocol::{receive_fd, Event, Request, MAX_FRAME_BYTES};
use ironrdp_server::{
    BitmapUpdate, CredentialDecision, CredentialValidationError, CredentialValidator, Credentials,
    DesktopSize, DisplayUpdate, PixelFormat, RdpServer, RdpServerDisplay, RdpServerDisplayUpdates,
    TlsIdentityCtx,
};
use rand::RngCore;
use rcgen::{generate_simple_self_signed, CertifiedKey};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{info, warn};
use zeroize::Zeroizing;

const DEFAULT_ADDR: &str = "127.0.0.1:3389";
const TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const USERNAME: &str = "focaldesk";

#[derive(Clone)]
struct RemoteFrame {
    serial: u64,
    width: u16,
    height: u16,
    stride: NonZeroUsize,
    pixels: Bytes,
}

struct DisplayHandler {
    initial_size: DesktopSize,
    frames: watch::Receiver<Option<RemoteFrame>>,
}

struct DisplayUpdates {
    size: DesktopSize,
    frames: watch::Receiver<Option<RemoteFrame>>,
    last_serial: Option<u64>,
    pending_after_resize: Option<RemoteFrame>,
}

#[async_trait::async_trait]
impl RdpServerDisplay for DisplayHandler {
    async fn size(&mut self) -> DesktopSize {
        self.initial_size
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(DisplayUpdates {
            size: self.initial_size,
            frames: self.frames.clone(),
            last_serial: None,
            pending_after_resize: None,
        }))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for DisplayUpdates {
    async fn next_update(&mut self) -> anyhow::Result<Option<DisplayUpdate>> {
        if let Some(frame) = self.pending_after_resize.take() {
            self.last_serial = Some(frame.serial);
            return Ok(Some(DisplayUpdate::Bitmap(bitmap(frame))));
        }
        loop {
            if let Some(frame) = self.frames.borrow_and_update().clone() {
                if self.last_serial != Some(frame.serial) {
                    let next_size = DesktopSize {
                        width: frame.width,
                        height: frame.height,
                    };
                    if next_size != self.size {
                        self.size = next_size;
                        self.pending_after_resize = Some(frame);
                        return Ok(Some(DisplayUpdate::Resize(next_size)));
                    }
                    self.last_serial = Some(frame.serial);
                    return Ok(Some(DisplayUpdate::Bitmap(bitmap(frame))));
                }
            }
            if self.frames.changed().await.is_err() {
                return Ok(None);
            }
        }
    }
}

fn bitmap(frame: RemoteFrame) -> BitmapUpdate {
    BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(frame.width).expect("validated frame width"),
        height: NonZeroU16::new(frame.height).expect("validated frame height"),
        format: PixelFormat::BgrA32,
        data: frame.pixels,
        stride: frame.stride,
    }
}

struct ExpiringTokenValidator {
    token: Zeroizing<String>,
    expires_at: Instant,
}

#[async_trait::async_trait]
impl CredentialValidator for ExpiringTokenValidator {
    async fn validate(
        &self,
        credentials: &Credentials,
    ) -> Result<CredentialDecision, CredentialValidationError> {
        let valid = Instant::now() < self.expires_at
            && credentials.username == USERNAME
            && constant_time_eq(credentials.password.as_bytes(), self.token.as_bytes());
        Ok(if valid {
            CredentialDecision::Accept
        } else {
            CredentialDecision::Reject
        })
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[derive(Serialize)]
struct TokenDocument<'a> {
    username: &'a str,
    token: &'a str,
    expires_unix_seconds: u64,
}

struct Config {
    bind_addr: SocketAddr,
    output_id: u64,
}

fn parse_config() -> Result<Config> {
    let mut enabled = false;
    let mut bind_addr: SocketAddr = DEFAULT_ADDR.parse().expect("valid default address");
    let mut output_id = 1;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--enable" => enabled = true,
            "--bind" => {
                bind_addr = args.next().context("--bind requires an address")?.parse()?;
            }
            "--output" => {
                output_id = args.next().context("--output requires an id")?.parse()?;
            }
            "--help" | "-h" => {
                println!("focaldesk-remoted --enable [--bind 127.0.0.1:3389] [--output 1]");
                std::process::exit(0);
            }
            unknown => bail!("unknown argument: {unknown}"),
        }
    }
    if !enabled {
        bail!("remote desktop is disabled; pass --enable to start it explicitly");
    }
    validate_bind_addr(bind_addr)?;
    Ok(Config {
        bind_addr,
        output_id,
    })
}

fn validate_bind_addr(bind_addr: SocketAddr) -> Result<()> {
    if !bind_addr.ip().is_loopback() {
        bail!("Phase 3 permits loopback listeners only");
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "focaldesk_remoted=info,ironrdp_server=warn".into()),
        )
        .init();
    let config = parse_config()?;
    let runtime_dir = runtime_directory()?;
    let (cert_path, key_path) = generate_ephemeral_identity(&runtime_dir)?;
    // IronRDP's helper honors SSLKEYLOGFILE. Development remote desktop must
    // never emit session secrets, even if inherited from a debugging shell.
    std::env::remove_var("SSLKEYLOGFILE");
    let identity = TlsIdentityCtx::init_from_paths(&cert_path, &key_path)?;
    let tls = identity.make_acceptor()?;

    let token = generate_token();
    let expires_at = Instant::now() + TOKEN_LIFETIME;
    write_token(&runtime_dir, &token, TOKEN_LIFETIME)?;

    let (stream, started) = connect_capture(config.output_id)?;
    let Event::CaptureStarted { width, height, .. } = started else {
        unreachable!();
    };
    let initial_size = checked_size(width, height)?;
    let (frames_tx, frames_rx) = watch::channel(None);
    spawn_capture_reader(stream, config.output_id, frames_tx)?;

    let display = DisplayHandler {
        initial_size,
        frames: frames_rx,
    };
    let validator = Arc::new(ExpiringTokenValidator {
        token: Zeroizing::new(token),
        expires_at,
    });
    let mut server = RdpServer::builder()
        .with_addr(config.bind_addr)
        .with_tls(tls)
        .with_no_input()
        .with_display_handler(display)
        .with_credential_validator(Some(validator))
        .build();

    info!(address = %config.bind_addr, output = config.output_id, "view-only RDP over TLS enabled");
    server.run().await.context("RDP server stopped")
}

fn runtime_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let path = PathBuf::from(base).join("focaldesk");
    std::fs::create_dir_all(&path)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("runtime path is not a real directory");
    }
    // SAFETY: geteuid has no preconditions and only reads process credentials.
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!("runtime directory is not owned by the current user");
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn capture_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FOCALDESK_REMOTE_SOCKET_PATH") {
        return Ok(path.into());
    }
    Ok(runtime_directory()?.join("remote-capture.sock"))
}

fn connect_capture(output_id: u64) -> Result<(UnixStream, Event)> {
    let stream = UnixStream::connect(capture_socket_path()?)
        .context("connect to compositor capture socket")?;
    focaldesk_remote_protocol::write_message(&stream, &Request::StartCapture { output_id })?;
    let event = focaldesk_remote_protocol::read_message::<Event>(&stream)?;
    match event {
        Event::CaptureStarted { .. } => Ok((stream, event)),
        Event::Error { ref message } => bail!("compositor rejected capture: {message}"),
        _ => bail!("unexpected compositor response"),
    }
}

fn spawn_capture_reader(
    initial_stream: UnixStream,
    output_id: u64,
    frames: watch::Sender<Option<RemoteFrame>>,
) -> Result<()> {
    thread::Builder::new()
        .name("focaldesk-remoted-capture".into())
        .spawn(move || {
            let mut stream = Some(initial_stream);
            let serial = AtomicU64::new(1);
            loop {
                let active = match stream.take() {
                    Some(stream) => stream,
                    None => match connect_capture(output_id) {
                        Ok((stream, _)) => stream,
                        Err(error) => {
                            warn!(%error, "waiting for compositor capture transport");
                            thread::sleep(RECONNECT_DELAY);
                            continue;
                        }
                    },
                };
                if let Err(error) = read_capture_frames(&active, &frames, &serial) {
                    warn!(%error, "compositor capture disconnected");
                    frames.send_replace(None);
                    thread::sleep(RECONNECT_DELAY);
                }
            }
        })?;
    Ok(())
}

fn read_capture_frames(
    stream: &UnixStream,
    frames: &watch::Sender<Option<RemoteFrame>>,
    serial: &AtomicU64,
) -> Result<()> {
    loop {
        match focaldesk_remote_protocol::read_message::<Event>(stream)? {
            Event::FrameReady {
                frame_serial: _,
                width,
                height,
                stride,
                len,
                ..
            } => {
                let width = u16::try_from(width).context("frame width exceeds RDP limit")?;
                let height = u16::try_from(height).context("frame height exceeds RDP limit")?;
                let _ = checked_size(u32::from(width), u32::from(height))?;
                let expected = u64::from(stride)
                    .checked_mul(u64::from(height))
                    .context("frame size overflow")?;
                if len != expected || len > MAX_FRAME_BYTES {
                    bail!("invalid shared frame length");
                }
                let fd = receive_fd(stream)?;
                let mut file = File::from(fd);
                let mut pixels = vec![0_u8; len as usize];
                file.read_exact(&mut pixels)?;
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                let stride = NonZeroUsize::new(stride as usize).context("zero frame stride")?;
                frames.send_replace(Some(RemoteFrame {
                    // Keep the RDP-side sequence monotonic across compositor
                    // restarts, where compositor frame serials begin again.
                    serial: serial.fetch_add(1, Ordering::Relaxed),
                    width,
                    height,
                    stride,
                    pixels: pixels.into(),
                }));
            }
            Event::CaptureStopped { .. } => bail!("capture stopped"),
            Event::Error { message } => bail!(message),
            Event::CaptureStarted { .. } => {}
        }
    }
}

fn checked_size(width: u32, height: u32) -> Result<DesktopSize> {
    let width = u16::try_from(width).context("output width exceeds RDP limit")?;
    let height = u16::try_from(height).context("output height exceeds RDP limit")?;
    if width == 0 || height == 0 {
        bail!("output has zero dimensions");
    }
    Ok(DesktopSize { width, height })
}

fn generate_ephemeral_identity(runtime: &Path) -> Result<(PathBuf, PathBuf)> {
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec![
        "localhost".into(),
        IpAddr::V4(Ipv4Addr::LOCALHOST).to_string(),
    ])?;
    let cert_path = runtime.join("remoted-cert.pem");
    let key_path = runtime.join("remoted-key.pem");
    write_private(&cert_path, cert.pem().as_bytes())?;
    write_private(&key_path, signing_key.serialize_pem().as_bytes())?;
    Ok((cert_path, key_path))
}

fn generate_token() -> String {
    let mut bytes = Zeroizing::new([0_u8; 24]);
    rand::thread_rng().fill_bytes(bytes.as_mut());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_token(runtime: &Path, token: &str, lifetime: Duration) -> Result<()> {
    let expires = SystemTime::now()
        .checked_add(lifetime)
        .context("token expiry overflow")?
        .duration_since(UNIX_EPOCH)?
        .as_secs();
    let document = serde_json::to_vec_pretty(&TokenDocument {
        username: USERNAME,
        token,
        expires_unix_seconds: expires,
    })?;
    write_private(&runtime.join("remoted-token.json"), &document)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Seek;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn constant_time_comparison_handles_equal_and_different_lengths() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"secret-long"));
    }

    #[test]
    fn configuration_rejects_non_loopback_address() {
        assert!(validate_bind_addr("0.0.0.0:3389".parse().unwrap()).is_err());
        assert!(validate_bind_addr("127.0.0.1:3389".parse().unwrap()).is_ok());
    }

    #[test]
    fn validates_rdp_dimensions() {
        assert!(checked_size(1920, 1080).is_ok());
        assert!(checked_size(0, 1080).is_err());
        assert!(checked_size(70_000, 1080).is_err());
    }

    #[test]
    fn private_files_are_owner_only_and_refuse_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("credential.json");
        write_private(&private, b"secret").unwrap();

        let mode = std::fs::metadata(&private).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let link = directory.path().join("credential-link.json");
        symlink(&private, &link).unwrap();
        assert!(write_private(&link, b"replacement").is_err());
        assert_eq!(std::fs::read(&private).unwrap(), b"secret");
    }

    #[test]
    fn shared_rgba_frame_becomes_bgra_rdp_frame() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let file = tempfile::tempfile().unwrap();
        (&file).write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        (&file).rewind().unwrap();
        let writer = thread::spawn(move || {
            focaldesk_remote_protocol::write_message(
                &sender,
                &Event::FrameReady {
                    session_id: 1,
                    frame_serial: 99,
                    width: 2,
                    height: 1,
                    stride: 8,
                    len: 8,
                    format: focaldesk_remote_protocol::PixelFormat::Rgba8888,
                    scale: 1.0,
                    transform: focaldesk_remote_protocol::OutputTransform::Normal,
                    full_refresh: true,
                },
            )
            .unwrap();
            focaldesk_remote_protocol::send_fd(&sender, file.as_raw_fd()).unwrap();
        });
        let (frames, received) = watch::channel(None);
        let serial = AtomicU64::new(7);
        assert!(read_capture_frames(&receiver, &frames, &serial).is_err());
        writer.join().unwrap();
        let frame = received.borrow().clone().unwrap();
        assert_eq!(frame.serial, 7);
        assert_eq!(frame.pixels.as_ref(), &[3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
