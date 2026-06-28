use focaldesk_types::OutputId;
use smithay::utils::{Logical, Point};

/// Key press/release state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKeyState {
    Pressed,
    Released,
}

/// Mouse buttons (backend-independent)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowMouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u16),
}

/// Modifier keys snapshot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlowModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

/// Scroll delta (line vs pixel precise)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowScrollDelta {
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
        source: FlowScrollSource,
        x_inverted: bool,
        y_inverted: bool,
        stop_x: bool,
        stop_y: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowScrollSource {
    Finger,
    Continuous,
    Wheel,
    WheelTilt,
}

/// Core FocalDesk input event
#[derive(Debug, Clone, PartialEq)]
pub enum FlowInputEvent {
    Key {
        keycode: u32,
        state: FlowKeyState,
        repeat: bool,
        modifiers: FlowModifiers,
    },

    PointerMoved {
        position: Point<f64, Logical>,
        delta_unaccel: Option<Point<f64, Logical>>,
    },

    PointerButton {
        button: FlowMouseButton,
        state: FlowKeyState,
        position: Point<f64, Logical>,
    },

    PointerScroll {
        delta: FlowScrollDelta,
        position: Point<f64, Logical>,
    },

    PointerEntered,
    PointerLeft,

    Resized {
        output_id: OutputId,
        width: u32,
        height: u32,
        scale_factor: f64,
    },

    CloseRequested,
}

/// Runtime input snapshot (lives in DesktopState)
#[derive(Debug, Clone)]
pub struct InputState {
    pub pointer_pos: Point<f64, Logical>,
    pub modifiers: FlowModifiers,
    /// True while the left mouse button is held (winit path; used for compositor move threshold).
    pub pointer_left_down: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            pointer_pos: Point::from((0.0, 0.0)),
            modifiers: FlowModifiers::default(),
            pointer_left_down: false,
        }
    }
}
