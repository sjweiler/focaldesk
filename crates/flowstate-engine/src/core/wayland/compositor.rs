use crate::core::wayland::client::ClientState;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::wayland::compositor::CompositorClientState;
use smithay::delegate_compositor;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    CompositorHandler,
    CompositorState as SmithayCompositorState,
};

use smithay::desktop::layer_map_for_output;

use smithay::reexports::wayland_server::Client;
use crate::core::desktop::DesktopState;


impl CompositorHandler for DesktopState {
    fn compositor_state(&mut self) -> &mut SmithayCompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<DesktopState>(surface);
        self.handle_commit(surface);
        for output in self.space.outputs() {
            layer_map_for_output(output).arrange();
        }
    }
}

delegate_compositor!(DesktopState);
