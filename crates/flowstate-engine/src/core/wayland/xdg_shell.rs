use smithay::delegate_xdg_shell;
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
use flowstate_logging::flog;
#[allow(unused_imports)]
use flowstate_types::WindowId;

impl XdgShellHandler for DesktopState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.add_xdg_toplevel(surface.clone());
        surface.with_pending_state(|state| {
            state
                .states
                .set(wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
            state.size = Some((1280, 720).into());
        });
        surface.send_configure();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        use wayland_server::Resource;

        self.unconstrain_popup(&surface);
        flog(&format!(
            "xdg popup new surface={:?}",
            surface.wl_surface().id()
        ));
        if let Err(e) = self.popups.track_popup(PopupKind::from(surface.clone())) {
            flog(&format!("Failed to track xdg popup: {e:?}"));
        }
        if !surface.is_initial_configure_sent() {
            if let Err(e) = surface.send_configure() {
                flog(&format!("Failed to configure xdg popup: {e:?}"));
            } else {
                flog(&format!(
                    "xdg popup initial configure sent surface={:?}",
                    surface.wl_surface().id()
                ));
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
        let Some(seat) = Seat::<DesktopState>::from_resource(&seat) else {
            flog(&format!(
                "xdg popup grab ignored surface={surface_id:?}: unknown seat"
            ));
            return;
        };
        let kind = PopupKind::from(surface);
        let Some(root) = find_popup_root_surface(&kind).ok() else {
            flog(&format!(
                "xdg popup grab ignored surface={surface_id:?}: no root"
            ));
            return;
        };
        let Some(window) = self.window_for_wl_surface(&root) else {
            flog(&format!(
                "xdg popup grab ignored surface={surface_id:?}: root window not mapped"
            ));
            return;
        };
        let root_focus = KeyboardFocusTarget::Window(window.clone());
        let Ok(mut grab) = self.popups.grab_popup(root_focus, kind, &seat, serial) else {
            flog(&format!(
                "xdg popup grab ignored surface={surface_id:?}: grab_popup failed"
            ));
            return;
        };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                flog(&format!(
                    "xdg popup grab rejected surface={surface_id:?}: keyboard serial mismatch"
                ));
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
                flog(&format!(
                    "xdg popup grab rejected surface={surface_id:?}: pointer serial mismatch"
                ));
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        if self.input.pointer_left_down {
            self.suppress_next_left_release();
        }
        flog(&format!(
            "xdg popup grab active surface={surface_id:?} serial={serial:?}"
        ));
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

        flog(&format!(
            "xdg popup destroyed surface={:?}",
            surface.wl_surface().id()
        ));
        self.mark_focused_output_full_damage(crate::core::desktop::DamageSource::CommitBbox);
    }
}

delegate_xdg_shell!(DesktopState);
