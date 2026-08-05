//! Reservation discovery for FocalDesk's trusted shell clients.
//!
//! Layer-shell already has a compositor-defined exclusive-zone mechanism. This
//! module narrows that mechanism to the two FocalDesk namespaces so an unrelated
//! layer-shell client cannot silently move the desktop work area.

use smithay::{
    desktop::layer_map_for_output,
    output::Output,
    utils::{Logical, Rectangle},
    wayland::shell::wlr_layer::{Anchor, ExclusiveZone},
};

pub const PANEL_NAMESPACE: &str = "focal-panel";
pub const DOCK_NAMESPACE: &str = "focal-dock";

pub fn is_trusted_namespace(namespace: &str) -> bool {
    matches!(namespace, PANEL_NAMESPACE | DOCK_NAMESPACE)
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
}
