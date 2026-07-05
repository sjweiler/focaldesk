use std::os::fd::OwnedFd;

use smithay::utils::{Logical, Rectangle};
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge as X11ResizeEdge, WmWindowProperty, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::core::desktop::DesktopState;
use crate::core::focus::KeyboardFocusTarget;
use crate::core::wayland::data_device::ClipboardSelectionOwner;
use focaldesk_logging::session_id;
use tracing::{debug, info_span, trace};

impl XWaylandShellHandler for DesktopState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        let known_window = self.window_id_for_x11_surface(&surface).is_some();
        self.sync_xwayland_window_meta(&surface);
        if known_window {
            self.map_xwayland_window(surface);
            return;
        }
        self.mark_focused_output_full_damage(crate::core::desktop::DamageSource::Unknown);
    }
}

impl XwmHandler for DesktopState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm
            .as_mut()
            .expect("XWayland WM not ready — Wayland clients were dispatched before X11Wm::start_wm completed")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.map_xwayland_window(window);
    }

    fn map_window_notify(&mut self, _xwm: XwmId, window: X11Surface) {
        let damaged_id = self
            .windows
            .iter()
            .find(|managed| {
                managed
                    .window
                    .x11_surface()
                    .is_some_and(|x11| x11 == &window)
            })
            .map(|managed| managed.id);
        if let Some(id) = damaged_id {
            self.mark_window_id_damage(id, crate::core::desktop::DamageSource::CommitBbox);
            if let Some(managed) = self.window(id).map(|managed| managed.window.clone()) {
                managed.on_commit();
            }
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let window_id = self.window_id_for_x11_surface(&window);
        let _span = info_span!(
            "xwayland_override_redirect_mapped",
            session_id = session_id(),
            window_id = ?window_id,
            title = ?window.title(),
            class = ?window.class(),
            geometry = ?window.geometry()
        )
        .entered();
        trace!(target: "focaldesk", "xwayland override-redirect mapped");
        self.map_xwayland_window(window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let window_id = self.window_id_for_x11_surface(&window);
        let _span = info_span!(
            "xwayland_window_unmapped",
            session_id = session_id(),
            window_id = ?window_id,
            override_redirect = window.is_override_redirect(),
            title = ?window.title(),
            class = ?window.class(),
            geometry = ?window.geometry()
        )
        .entered();
        debug!(target: "focaldesk", "xwayland window unmapped");
        let Some(id) = window_id else {
            return;
        };
        self.mark_window_id_damage(id, crate::core::desktop::DamageSource::CommitBbox);
        if let Some(idx) = self.windows.iter().position(|managed| managed.id == id) {
            let managed = self.windows.remove(idx);
            self.space.unmap_elem(&managed.window);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
        if self.focused_window == Some(id) {
            self.focused_window = None;
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let window_id = self.window_id_for_x11_surface(&window);
        let _span = info_span!(
            "xwayland_window_destroyed",
            session_id = session_id(),
            window_id = ?window_id,
            override_redirect = window.is_override_redirect(),
            title = ?window.title(),
            class = ?window.class(),
            geometry = ?window.geometry()
        )
        .entered();
        let Some(id) = window_id else {
            return;
        };
        self.mark_window_id_damage(id, crate::core::desktop::DamageSource::CommitBbox);
        if let Some(idx) = self.windows.iter().position(|managed| managed.id == id) {
            let managed = self.windows.remove(idx);
            self.space.unmap_elem(&managed.window);
        }
        if self.focused_window == Some(id) {
            self.focused_window = None;
        }
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geometry = window.geometry();
        if let Some(w) = w {
            geometry.size.w = w as i32;
        }
        if let Some(h) = h {
            geometry.size.h = h as i32;
        }

        if window.is_override_redirect() {
            if let Some(x) = x {
                geometry.loc.x = x;
            }
            if let Some(y) = y {
                geometry.loc.y = y;
            }
            let output_id = self
                .window_id_for_x11_surface(&window)
                .map(|window_id| self.xwayland_output_id_for_window(window_id))
                .unwrap_or_else(|| {
                    self.output_under_pointer(self.input.pointer_pos)
                        .unwrap_or(self.primary_output)
                });
            geometry = self.xwayland_clamp_override_redirect_geometry(output_id, geometry);
            let _ = window.configure(geometry);
            return;
        }

        if let Some(id) = self.window_id_for_x11_surface(&window) {
            let output_id = self.xwayland_output_id_for_window(id);
            if self.xwayland_request_fills_output(output_id, geometry.size) {
                self.set_window_maximized(id, true);
                return;
            }
            geometry = self.xwayland_clamp_toplevel_geometry(output_id, geometry, Some(id));
            let _ = window.configure(geometry);
            return;
        }

        let output_id = self
            .output_under_pointer(self.input.pointer_pos)
            .unwrap_or(self.primary_output);
        if self.xwayland_request_fills_output(output_id, geometry.size) {
            if let Some(work) = self.work_recess_for_output(output_id) {
                let _ = window.configure(work);
                return;
            }
        }
        geometry = self.xwayland_clamp_toplevel_geometry(output_id, geometry, None);
        let _ = window.configure(geometry);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        let window_id = self.window_id_for_x11_surface(&window);
        let Some(id) = window_id else {
            return;
        };
        let _span = info_span!(
            "xwayland_configure_notify",
            session_id = session_id(),
            window_id = ?window_id,
            geometry = ?geometry
        )
        .entered();
        let Some(managed) = self.window(id).map(|managed| managed.window.clone()) else {
            return;
        };
        self.mark_window_id_damage(id, crate::core::desktop::DamageSource::WindowResize);
        if window.is_override_redirect() {
            let compositor_loc = self.xwayland_or_compositor_loc(&window, geometry.loc);
            let rect = self.xwayland_clamp_override_redirect_geometry(
                self.xwayland_output_id_for_window(id),
                Rectangle::from_loc_and_size(compositor_loc, geometry.size),
            );
            if let Some(state) = self.window_mut(id) {
                state.float_rect = Some(rect);
            }
            self.map_window_bbox_location(managed, rect.loc, false);
        } else {
            let output_id = self.xwayland_output_id_for_window(id);
            let fills_output = self.xwayland_request_fills_output(output_id, geometry.size);
            let maximized = self
                .window(id)
                .map(|state| state.maximized)
                .unwrap_or(false);

            if fills_output && !maximized {
                self.set_window_maximized(id, true);
                return;
            }

            let rect = if maximized {
                self.work_recess_for_output(output_id).unwrap_or(geometry)
            } else {
                let current_loc = self.xwayland_compositor_loc_for_window(id);
                Rectangle::from_loc_and_size(current_loc, geometry.size)
            };
            if let Some(state) = self.window_mut(id) {
                state.float_rect = Some(rect);
            }
            self.map_window_bbox_location(managed, rect.loc, false);
            if maximized && geometry != rect {
                let _ = window.configure(rect);
            }
        }
        self.space.refresh();
        self.mark_window_id_damage(id, crate::core::desktop::DamageSource::WindowResize);
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, _property: WmWindowProperty) {
        self.sync_xwayland_window_meta(&window);
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.mark_window_id_damage(id, crate::core::desktop::DamageSource::Unknown);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.request_maximize(id);
            let _ = window.set_maximized(true);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.set_window_maximized(id, false);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.set_window_fullscreen(id, true, None);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.request_unfullscreen(id);
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        edge: X11ResizeEdge,
    ) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.request_resize(id, x11_resize_edge_to_xdg(edge));
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.queue_deferred_move(id);
        }
    }

    fn active_window_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _timestamp: u32,
        _currently_active_window: Option<X11Surface>,
    ) {
        if let Some(id) = self.window_id_for_x11_surface(&window) {
            self.focus_window_id(id);
        }
    }

    fn allow_selection_access(&mut self, xwm: XwmId, selection: SelectionTarget) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let Some(KeyboardFocusTarget::Window(window)) = keyboard.current_focus() else {
            return false;
        };
        let Some(surface) = window.x11_surface() else {
            return false;
        };
        matches!(
            selection,
            SelectionTarget::Clipboard | SelectionTarget::Primary
        ) && surface.xwm_id().map(|id| id == xwm).unwrap_or(false)
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    focaldesk_logging::flog(&format!(
                        "failed to request Wayland clipboard for XWayland: {err}"
                    ));
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    focaldesk_logging::flog(&format!(
                        "failed to request Wayland primary selection for XWayland: {err}"
                    ));
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(
                    &self.display_handle,
                    &self.seat,
                    mime_types,
                    ClipboardSelectionOwner::XWayland,
                );
            }
            SelectionTarget::Primary => {
                set_primary_selection(
                    &self.display_handle,
                    &self.seat,
                    mime_types,
                    ClipboardSelectionOwner::XWayland,
                );
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat);
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat);
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        self.disable_xwayland();
    }
}

fn x11_resize_edge_to_xdg(
    edge: X11ResizeEdge,
) -> wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge {
    use wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;

    match edge {
        X11ResizeEdge::Top => ResizeEdge::Top,
        X11ResizeEdge::Bottom => ResizeEdge::Bottom,
        X11ResizeEdge::Left => ResizeEdge::Left,
        X11ResizeEdge::Right => ResizeEdge::Right,
        X11ResizeEdge::TopLeft => ResizeEdge::TopLeft,
        X11ResizeEdge::TopRight => ResizeEdge::TopRight,
        X11ResizeEdge::BottomLeft => ResizeEdge::BottomLeft,
        X11ResizeEdge::BottomRight => ResizeEdge::BottomRight,
    }
}
