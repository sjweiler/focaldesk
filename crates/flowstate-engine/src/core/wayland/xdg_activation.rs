use smithay::delegate_xdg_activation;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler,
    XdgActivationState,
    XdgActivationToken,
    XdgActivationTokenData,
};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::core::desktop::DesktopState;

impl XdgActivationHandler for DesktopState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(id) = self.window_id_for_wl_surface(&surface) {
            self.focus_window_id(id);
            self.mark_redraw();
        }

        let _ = self.xdg_activation_state.remove_token(&token);
    }
}

delegate_xdg_activation!(DesktopState);
