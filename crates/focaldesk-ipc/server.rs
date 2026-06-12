// ip server stuff goes here

// crates/compositor/src/ipc/mod.rs
pub mod proto;

use std::{
    collections::HashMap,
    io,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use calloop::{
    generic::{Generic, GenericEvent},
    Interest, Mode, PostAction,
};
use nix::sys::socket::UnixCredentials;

use crate::{actions::Action, App}; // your compositor state

#[derive(Debug)]
struct Client {
    stream: UnixStream,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
}

pub struct IpcServer {
    _path: PathBuf,
    listener: UnixListener,
    clients: HashMap<u64, Client>,
    next_id: u64,
}

impl IpcServer {
    pub fn bind_runtime() -> io::Result<Self> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR not set"))?;

        let flowos_dir = dir.join("flowos");
        std::fs::create_dir_all(&flowos_dir)?;
        // tighten perms: runtime dirs are usually 0700 already, but be explicit
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&flowos_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        let sock_path = flowos_dir.join("ipc.sock");
        // remove stale socket from crash
        let _ = std::fs::remove_file(&sock_path);

        let listener = UnixListener::bind(&sock_path)?;
        listener.set_nonblocking(true)?;

        Ok(Self {
            _path: sock_path,
            listener,
            clients: HashMap::new(),
            next_id: 1,
        })
    }

    pub fn register(self, event_loop: &mut calloop::EventLoop<'static, App>) -> io::Result<()> {
        let handle = event_loop.handle();

        // 1) Listener source (accept new connections)
        handle.insert_source(
            Generic::new(self.listener, Interest::READ, Mode::Level),
            |GenericEvent::Read, listener, _meta, app: &mut App| {
                loop {
                    match listener.accept() {
                        Ok((mut stream, _addr)) => {
                            let _ = stream.set_nonblocking(true);

                            // Optional: credential check (Linux): allow only same UID
                            if let Ok(ucred) = get_peer_cred(&stream) {
                                if ucred.uid() != nix::unistd::Uid::current().as_raw() {
                                    // reject
                                    let _ = std::io::Write::write_all(&mut stream, b"");
                                    // drop stream by not storing it
                                    continue;
                                }
                            }

                            app.ipc_add_client(stream);
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::warn!(?e, "ipc accept error");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )?;

        // We can’t move `self` into the calloop closure easily without putting server into `App`.
        // So: store server in `App` and implement `ipc_add_client` there (shown below).

        Ok(())
    }
}

fn get_peer_cred(_stream: &UnixStream) -> nix::Result<UnixCredentials> {
    // On Linux you can use SO_PEERCRED via nix; exact method varies by nix version.
    // Keep as optional; perms on the socket already do most of the work.
    Err(nix::Error::ENOSYS)
}
