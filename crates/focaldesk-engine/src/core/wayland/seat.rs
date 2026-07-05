use focaldesk_flow::keybinds::BackendKind;
use smithay::input::{
    pointer::{CursorIcon, CursorImageStatus},
    Seat, SeatHandler, SeatState,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::set_data_device_focus;
use smithay::wayland::selection::primary_selection::set_primary_focus;
use wayland_server::Resource;

use crate::core::desktop::DesktopState;
use crate::core::focus::{KeyboardFocusTarget, PointerFocusTarget};
use smithay::wayland::tablet_manager::TabletSeatHandler;

impl TabletSeatHandler for DesktopState {}

impl SeatHandler for DesktopState {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.drm_submit_hw_cursor = true;
        match image {
            // GTK/XWayland often hides the seat cursor while using a subsurface cursor; keep the
            // compositor fallback visible on DRM so the pointer does not disappear entirely.
            CursorImageStatus::Hidden if self.backend_kind == BackendKind::Drm => {
                self.render.clear_sw_cursor_texture();
                self.cursor_manager.set_visible(true);
                self.cursor_manager.set_icon(CursorIcon::Default);
            }
            CursorImageStatus::Hidden => {
                self.render.clear_sw_cursor_texture();
                self.cursor_manager.set_visible(false);
            }
            CursorImageStatus::Named(icon) => {
                self.render.clear_sw_cursor_texture();
                self.cursor_manager.set_visible(true);
                self.cursor_manager.set_icon(icon);
                self.drm_submit_hw_cursor = true;
            }
            CursorImageStatus::Surface(_) => {
                // XWayland subsurface cursors: keep the theme cursor on the KMS plane.
                // Rendering client cursor surfaces in-frame was hiding the HW cursor entirely.
                self.render.clear_sw_cursor_texture();
                self.cursor_manager.set_visible(true);
                self.cursor_manager.set_icon(CursorIcon::Pointer);
                self.drm_submit_hw_cursor = true;
            }
        }
        self.mark_focused_output_full_damage(crate::core::desktop::DamageSource::Cursor);
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        let wl_surface = focused.and_then(WaylandFocus::wl_surface);
        let client =
            wl_surface.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
}

impl DesktopState {
    pub fn init_seat(
        &mut self,
        display_handle: &smithay::reexports::wayland_server::DisplayHandle,
    ) {
        let mut seat = self.seat_state.new_wl_seat(display_handle, "seat-0");
        seat.add_keyboard(Default::default(), 500, 20).unwrap();
        seat.add_pointer();
        //self.seat = seat;
    }
}
