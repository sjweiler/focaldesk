use smithay::desktop::{
    find_popup_root_surface, PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy,
};
use smithay::input::pointer::Focus;
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::utils::Serial;
use smithay::wayland::shell::xdg::{
    Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use wayland_server::protocol::{wl_output, wl_seat, wl_surface};

use crate::core::desktop::DesktopState;
use crate::core::focus::KeyboardFocusTarget;
use focaldesk_logging::session_id;
#[allow(unused_imports)]
use focaldesk_types::WindowId;
use tracing::{debug, info_span, trace};

impl XdgShellHandler for DesktopState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.add_xdg_toplevel(surface.clone());
        let output_id = self.focused_output;
        let size = self
            .work_recess_for_output(output_id)
            .map(|work| work.size)
            .unwrap_or_else(|| (1280, 720).into());
        surface.with_pending_state(|state| {
            state
                .states
                .set(wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
            state
                .states
                .set(wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized);
            state.size = Some(size);
        });
        surface.send_configure();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        use wayland_server::Resource;

        let _span = info_span!(
            "xdg_popup_new",
            session_id = session_id(),
            surface = ?surface.wl_surface().id()
        )
        .entered();
        self.unconstrain_popup(&surface);
        trace!(target: "focaldesk", "xdg popup new");
        if let Err(e) = self.popups.track_popup(PopupKind::from(surface.clone())) {
            debug!(target: "focaldesk", error = ?e, "failed to track xdg popup");
        }
        if !surface.is_initial_configure_sent() {
            if let Err(e) = surface.send_configure() {
                debug!(target: "focaldesk", error = ?e, "failed to configure xdg popup");
            } else {
                trace!(target: "focaldesk", "xdg popup initial configure sent");
            }
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        if !self.xdg_toplevel_pointer_grab_valid(surface.wl_surface(), serial) {
            return;
        }
        if let Some(id) = self.window_id_for_toplevel(&surface) {
            self.queue_xdg_move_request(id);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        use wayland_server::Resource;

        let surface_id = surface.wl_surface().id();
        let _span = info_span!(
            "xdg_popup_grab",
            session_id = session_id(),
            surface = ?surface_id,
            serial = ?serial
        )
        .entered();
        let Some(seat) = Seat::<DesktopState>::from_resource(&seat) else {
            debug!(target: "focaldesk", ?surface_id, "xdg popup grab ignored: unknown seat");
            return;
        };
        let kind = PopupKind::from(surface);
        let Some(root) = find_popup_root_surface(&kind).ok() else {
            debug!(target: "focaldesk", ?surface_id, "xdg popup grab ignored: no root");
            return;
        };
        let Some(window) = self.window_for_wl_surface(&root) else {
            debug!(
                target: "focaldesk",
                ?surface_id,
                "xdg popup grab ignored: root window not mapped"
            );
            return;
        };
        let root_focus = KeyboardFocusTarget::Window(window.clone());
        let Ok(mut grab) = self.popups.grab_popup(root_focus, kind, &seat, serial) else {
            debug!(target: "focaldesk", ?surface_id, "xdg popup grab ignored: grab_popup failed");
            return;
        };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                debug!(target: "focaldesk", ?surface_id, "xdg popup grab rejected: keyboard serial mismatch");
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                debug!(target: "focaldesk", ?surface_id, "xdg popup grab rejected: pointer serial mismatch");
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        if self.input.pointer_left_down {
            self.suppress_next_left_release();
        }
        trace!(target: "focaldesk", ?surface_id, ?serial, "xdg popup grab active");
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        if !self.xdg_toplevel_pointer_grab_valid(surface.wl_surface(), serial) {
            return;
        }
        if let Some(id) = self.window_id_for_toplevel(&surface) {
            self.request_resize(id, edges);
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id_for_toplevel(&surface) {
            self.request_maximize(id);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<wl_output::WlOutput>,
    ) {
        if let Some(id) = self.window_id_for_toplevel(&surface) {
            self.request_fullscreen(id, output);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id_for_toplevel(&surface) {
            self.request_unfullscreen(id);
        }
    }

    fn ack_configure(&mut self, _surface: wl_surface::WlSurface, _configure: Configure) {}

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        use wayland_server::Resource;

        let _span = info_span!(
            "xdg_popup_destroyed",
            session_id = session_id(),
            surface = ?surface.wl_surface().id()
        )
        .entered();
        trace!(target: "focaldesk", "xdg popup destroyed");
        self.mark_focused_output_full_damage(crate::core::desktop::DamageSource::CommitBbox);
    }
}
