//! Interactive XDG toplevel move/resize (winit input path; not Smithay pointer grabs).
use std::cell::RefCell;

use bitflags::bitflags;
use smithay::desktop::Space;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::compositor;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use wayland_protocols::xdg::shell::server::xdg_toplevel;

use flowstate_types::WindowId;

bitflags! {
    /// Bit-compatible with `xdg_toplevel::resize_edge` (top=1, bottom=2, left=4, right=8, corners OR'd).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResizeEdgeMask: u32 {
        const TOP = 1;
        const BOTTOM = 2;
        const LEFT = 4;
        const RIGHT = 8;
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdgeMask {
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits_truncate(x as u32)
    }
}

#[derive(Debug)]
pub enum ToplevelPointerInteraction {
    Move {
        window_id: WindowId,
        pointer_start: Point<f64, Logical>,
        initial_location: Point<i32, Logical>,
    },
    Resize {
        window_id: WindowId,
        edges: ResizeEdgeMask,
        pointer_start: Point<f64, Logical>,
        initial_rect: Rectangle<i32, Logical>,
        last_window_size: Size<i32, Logical>,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub(crate) enum ResizeSurfaceState {
    #[default]
    Idle,
    Resizing {
        edges: ResizeEdgeMask,
        initial_rect: Rectangle<i32, Logical>,
    },
    WaitingForLastCommit {
        edges: ResizeEdgeMask,
        initial_rect: Rectangle<i32, Logical>,
    },
}

impl ResizeSurfaceState {
    pub fn set_resizing(surface: &WlSurface, edges: ResizeEdgeMask, initial_rect: Rectangle<i32, Logical>) {
        Self::with(surface, |state| {
            *state = ResizeSurfaceState::Resizing { edges, initial_rect };
        });
    }

    pub fn set_waiting_for_commit(surface: &WlSurface, edges: ResizeEdgeMask, initial_rect: Rectangle<i32, Logical>) {
        Self::with(surface, |state| {
            *state = ResizeSurfaceState::WaitingForLastCommit { edges, initial_rect };
        });
    }

    fn with<F, T>(surface: &WlSurface, f: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing(|| RefCell::new(ResizeSurfaceState::Idle));
            let cell = states.data_map.get::<RefCell<ResizeSurfaceState>>().unwrap();
            f(&mut cell.borrow_mut())
        })
    }

    /// Called from the surface commit handler; matches smallvil's `ResizeSurfaceState::commit`.
    pub fn on_surface_commit(surface: &WlSurface) -> Option<(ResizeEdgeMask, Rectangle<i32, Logical>)> {
        Self::with(surface, |state| match *state {
            ResizeSurfaceState::Resizing { edges, initial_rect } => Some((edges, initial_rect)),
            ResizeSurfaceState::WaitingForLastCommit { edges, initial_rect } => {
                *state = ResizeSurfaceState::Idle;
                Some((edges, initial_rect))
            }
            ResizeSurfaceState::Idle => None,
        })
    }
}

/// After a client commit during/after resize, adjust space location for top/left drags.
pub fn handle_resize_surface_commit(space: &mut Space<Window>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    const TOP_LEFT: ResizeEdgeMask = ResizeEdgeMask::TOP.union(ResizeEdgeMask::LEFT);

    let new_loc: Point<Option<i32>, Logical> = ResizeSurfaceState::on_surface_commit(surface)
        .and_then(|(edges, initial_rect)| {
            // Same condition as smallvil: any top or left edge participation.
            edges.intersects(TOP_LEFT).then(|| {
                let new_x = edges
                    .intersects(ResizeEdgeMask::LEFT)
                    .then_some(initial_rect.loc.x + (initial_rect.size.w - geometry.size.w));
                let new_y = edges
                    .intersects(ResizeEdgeMask::TOP)
                    .then_some(initial_rect.loc.y + (initial_rect.size.h - geometry.size.h));
                (new_x, new_y).into()
            })
        })
        .unwrap_or_default();

    if let Some(x) = new_loc.x {
        window_loc.x = x;
    }
    if let Some(y) = new_loc.y {
        window_loc.y = y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        space.map_element(window, window_loc, false);
    }

    Some(())
}
