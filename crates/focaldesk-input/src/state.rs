use crate::{Modifiers, Position};

/// Runtime input snapshot stored by the compositor.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub pointer_pos: Position,
    pub modifiers: Modifiers,
    /// True while the left mouse button is held.
    pub pointer_left_down: bool,
    /// The primary contact currently driving compositor-native control-panel UI.
    ///
    /// Contacts on ordinary outputs continue through `wl_touch`; this is only
    /// used where the compositor itself owns the interactive surface.
    pub control_panel_touch_slot: Option<i32>,
}
