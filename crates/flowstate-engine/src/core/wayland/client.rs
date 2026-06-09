use flowstate_logging::flog_info;
use smithay::wayland::compositor::CompositorClientState;
use wayland_server::backend::{ClientData, ClientId, DisconnectReason};

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        flog_info!("initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        flog_info!("disconnected");
    }
}
