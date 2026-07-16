use smithay::input::pointer::PointerHandle;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler};
use smithay::wayland::seat::WaylandFocus;
use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
};

use crate::core::desktop::DesktopState;

impl PointerConstraintsHandler for DesktopState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };

        if current_focus.wl_surface().as_deref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn remove_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {}

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
        // The compositor keeps a locked pointer at its pre-lock location, so
        // unlocking already restores the cursor to a stable client position.
    }
}
