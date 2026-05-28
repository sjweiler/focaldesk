use smithay::delegate_seat;
use smithay::input::{
    pointer::CursorIcon, pointer::CursorImageStatus, Seat, SeatHandler, SeatState,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::set_data_device_focus;
use smithay::wayland::selection::primary_selection::set_primary_focus;
use wayland_server::Resource;

use crate::core::desktop::DesktopState;
use crate::core::focus::KeyboardFocusTarget;

impl SeatHandler for DesktopState {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.drm_submit_hw_cursor = true;
        match image {
            CursorImageStatus::Hidden => self.cursor_manager.set_visible(false),
            CursorImageStatus::Named(icon) => {
                self.cursor_manager.set_visible(true);
                self.cursor_manager.set_icon(icon);
            }
            CursorImageStatus::Surface(_) => {
                self.cursor_manager.set_visible(true);
                self.cursor_manager.set_icon(CursorIcon::Default);
            }
        }
        self.mark_redraw();
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        let wl_surface = focused.and_then(WaylandFocus::wl_surface);
        let client =
            wl_surface.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
}
delegate_seat!(DesktopState);

impl DesktopState {
    pub fn init_seat(
        &mut self,
        display_handle: &smithay::reexports::wayland_server::DisplayHandle,
    ) {
        let mut seat = self.seat_state.new_wl_seat(display_handle, "seat-0");
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();
        //self.seat = seat;
    }
}
