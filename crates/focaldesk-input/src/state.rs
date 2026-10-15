use crate::{Modifiers, Position};

/// Runtime input snapshot stored by the compositor.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub pointer_pos: Position,
    pub modifiers: Modifiers,
    /// True while the left mouse button is held.
    pub pointer_left_down: bool,
}
