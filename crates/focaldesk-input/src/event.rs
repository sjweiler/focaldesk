use crate::{Delta, Position};

/// Key press/release state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
}

/// Mouse buttons expressed independently of a specific input backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u16),
}

/// Modifier keys snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

/// Scroll delta normalized across line-, pixel-, and axis-based backends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollDelta {
    Line {
        x: f32,
        y: f32,
    },
    Pixel {
        x: f64,
        y: f64,
    },
    Axis {
        x: f64,
        y: f64,
        x_v120: Option<i32>,
        y_v120: Option<i32>,
        source: ScrollSource,
        x_inverted: bool,
        y_inverted: bool,
        stop_x: bool,
        stop_y: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Finger,
    Continuous,
    Wheel,
    WheelTilt,
}

/// Canonical event consumed by the FocalDesk compositor engine.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Key {
        keycode: u32,
        state: KeyState,
        repeat: bool,
        modifiers: Modifiers,
    },
    PointerMoved {
        position: Position,
        /// Raw accelerated device delta. Unlike `position`, this is not
        /// clamped to the desktop or divided by an output scale.
        delta: Option<Delta>,
        delta_unaccel: Option<Delta>,
    },
    PointerButton {
        button: MouseButton,
        state: KeyState,
        position: Position,
    },
    PointerScroll {
        delta: ScrollDelta,
        position: Position,
    },
    GestureSwipeBegin {
        fingers: u32,
        time_msec: u32,
    },
    GestureSwipeUpdate {
        delta: Delta,
        time_msec: u32,
    },
    GestureSwipeEnd {
        cancelled: bool,
        time_msec: u32,
    },
    PointerEntered,
    PointerLeft,
}
