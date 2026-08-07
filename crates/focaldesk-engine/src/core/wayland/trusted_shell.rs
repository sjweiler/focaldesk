//! Reservation discovery for FocalDesk's trusted shell clients.
//!
//! Layer-shell already has a compositor-defined exclusive-zone mechanism. This
//! module narrows that mechanism to the two FocalDesk namespaces so an unrelated
//! layer-shell client cannot silently move the desktop work area.

use smithay::{
    backend::renderer::utils::RendererSurfaceStateUserData,
    desktop::layer_map_for_output,
    output::Output,
    utils::{Logical, Point, Rectangle},
    wayland::compositor::with_states,
    wayland::shell::wlr_layer::{Anchor, ExclusiveZone},
};

pub const PANEL_NAMESPACE: &str = "focal-panel";
pub const DOCK_NAMESPACE: &str = "focal-dock";
pub const PANEL_INPUT_HEIGHT: i32 = 64;
pub const DOCK_INPUT_WIDTH: i32 = 76;

pub fn is_trusted_namespace(namespace: &str) -> bool {
    matches!(namespace, PANEL_NAMESPACE | DOCK_NAMESPACE)
}

/// The interactive part of each trusted shell surface. The clients allocate a
/// transparent extension for their tooltips, so their full layer geometry must
/// not become an input target. This is also used as a compositor-side fallback
/// when Smithay has not yet attached the committed `wl_region` to its renderer
/// surface view (notably after a viewport/fractional-scale transition).
pub fn input_region_contains(namespace: &str, point: Point<f64, Logical>) -> bool {
    match namespace {
        PANEL_NAMESPACE => point.x >= 0.0 && point.y >= 0.0 && point.y < PANEL_INPUT_HEIGHT as f64,
        DOCK_NAMESPACE => point.x >= 0.0 && point.x < DOCK_INPUT_WIDTH as f64 && point.y >= 0.0,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrustedShellReservation {
    pub top: i32,
    pub left: i32,
}

impl TrustedShellReservation {
    pub fn is_active(self) -> bool {
        self.top > 0 || self.left > 0
    }
}

/// Read exclusive zones claimed by the FocalDesk panel and dock on `output`.
///
/// This intentionally ignores all other layer-shell namespaces. The layer map
/// remains responsible for arranging and rendering every layer surface; this
/// helper only feeds trusted shell geometry into normal-window placement.
pub fn reservation_for_output(output: &Output) -> TrustedShellReservation {
    let map = layer_map_for_output(output);
    let mut reservation = TrustedShellReservation::default();

    for layer in map.layers() {
        // Do not let a configured-but-unpainted shell surface hide the
        // compositor's built-in chrome. Renderer state only gains a view after
        // a real client buffer has been imported successfully.
        let has_renderable_buffer = with_states(layer.wl_surface(), |states| {
            states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .and_then(|state| state.lock().ok())
                .and_then(|state| state.view())
                .is_some()
        });
        if !has_renderable_buffer {
            continue;
        }
        let zone = match layer.cached_state().exclusive_zone {
            ExclusiveZone::Exclusive(amount) if amount > 0 => amount,
            _ => continue,
        };
        let zone = zone.min(i32::MAX as u32) as i32;
        match layer.namespace() {
            PANEL_NAMESPACE if layer.cached_state().anchor.contains(Anchor::TOP) => {
                reservation.top = reservation.top.max(zone);
            }
            DOCK_NAMESPACE if layer.cached_state().anchor.contains(Anchor::LEFT) => {
                reservation.left = reservation.left.max(zone);
            }
            _ => {}
        }
    }
    reservation
}

/// Convert a trusted reservation into the output-local work area.
pub fn work_area_for_output(
    output: Rectangle<i32, Logical>,
    reservation: TrustedShellReservation,
) -> Rectangle<i32, Logical> {
    let left = reservation.left.clamp(0, output.size.w.saturating_sub(1));
    let top = reservation.top.clamp(0, output.size.h.saturating_sub(1));
    Rectangle::from_loc_and_size(
        (output.loc.x + left, output.loc.y + top),
        ((output.size.w - left).max(1), (output.size.h - top).max(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_applies_trusted_edges_without_underflow() {
        let output = Rectangle::from_loc_and_size((0, 0), (100, 80));
        assert_eq!(
            work_area_for_output(output, TrustedShellReservation { top: 20, left: 30 }),
            Rectangle::from_loc_and_size((30, 20), (70, 60))
        );
        assert_eq!(
            work_area_for_output(
                output,
                TrustedShellReservation {
                    top: 500,
                    left: 500
                }
            )
            .size,
            (1, 1).into()
        );
    }

    #[test]
    fn trusted_input_regions_exclude_tooltip_extensions() {
        assert!(input_region_contains(PANEL_NAMESPACE, (500.0, 32.0).into()));
        assert!(!input_region_contains(
            PANEL_NAMESPACE,
            (500.0, 70.0).into()
        ));
        assert!(input_region_contains(DOCK_NAMESPACE, (38.0, 500.0).into()));
        assert!(!input_region_contains(DOCK_NAMESPACE, (84.0, 500.0).into()));
        assert!(!input_region_contains("untrusted", (1.0, 1.0).into()));
    }
}
