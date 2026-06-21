use std::os::unix::net::UnixStream;

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use smithay::wayland::compositor::CompositorClientState;
use tracing::info_span;
use wayland_server::backend::{ClientData, ClientId, DisconnectReason};

use focaldesk_logging::session_id;

#[derive(Debug, Clone, Copy)]
pub struct ClientCredentials {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub credentials: Option<ClientCredentials>,
}

impl ClientState {
    pub fn from_stream(stream: &UnixStream) -> Self {
        let credentials = getsockopt(stream, PeerCredentials)
            .ok()
            .map(|creds| ClientCredentials {
                pid: creds.pid(),
                uid: creds.uid(),
                gid: creds.gid(),
            });

        Self {
            compositor_state: CompositorClientState::default(),
            credentials,
        }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        let span = info_span!(
            "wayland_client",
            session_id = session_id(),
            client_id = ?client_id
        );
        let _enter = span.enter();
        tracing::debug!("client initialized");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        let span = info_span!(
            "wayland_client",
            session_id = session_id(),
            client_id = ?client_id,
            reason = ?reason
        );
        let _enter = span.enter();
        tracing::debug!("client disconnected");
    }
}
