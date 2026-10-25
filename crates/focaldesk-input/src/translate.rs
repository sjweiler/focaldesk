use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Event,
    GestureBeginEvent, GestureEndEvent, GestureSwipeUpdateEvent, InputEvent as SmithayInputEvent,
    KeyState as SmithayKeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent, TouchEvent,
};
use smithay::utils::{Logical, Point as SmithayPoint, Rectangle, Size as SmithaySize, Transform};

use crate::{
    transform_absolute_position, Bounds, Delta, InputEvent, KeyState, Modifiers, MouseButton,
    OutputTransform, Position, ScrollDelta, ScrollSource, Size,
};

/// Converts a Smithay backend event into FocalDesk's canonical input model.
pub fn translate_backend_input<B: smithay::backend::input::InputBackend>(
    input: &SmithayInputEvent<B>,
    pointer_pos: SmithayPoint<f64, Logical>,
    clamp_rect: Rectangle<i32, Logical>,
    output_transform: Transform,
    scale_factor: f64,
    modifiers: Modifiers,
    pointer_speed: f32,
    natural_scroll: bool,
) -> Option<InputEvent> {
    match input {
        SmithayInputEvent::Keyboard { event, .. } => {
            let state = key_state(event.state());

            Some(InputEvent::Key {
                keycode: event.key_code().into(),
                state,
                repeat: false,
                modifiers,
            })
        }
        SmithayInputEvent::PointerMotion { event, .. } => {
            let pointer_pos = Position::from(pointer_pos);
            let speed = f64::from(pointer_speed.clamp(0.25, 2.0));
            let raw_delta = event.delta();
            let delta = Delta::new(raw_delta.x * speed, raw_delta.y * speed);
            let position =
                relative_pointer_position(pointer_pos, delta, scale_factor, clamp_rect.into());

            Some(InputEvent::PointerMoved {
                position,
                delta: Some(delta),
                delta_unaccel: Some(event.delta_unaccel().into()),
            })
        }
        SmithayInputEvent::PointerMotionAbsolute { event, .. } => {
            // Absolute devices report coordinates in the panel's unrotated coordinate space.
            // Map that space through the output transform so a tablet mapped to a portrait
            // output follows the pixels instead of retaining landscape axes.
            let source_size = output_transform.invert().transform_size(clamp_rect.size);
            let local = transform_absolute_position(
                event.position_transformed(source_size).to_f64().into(),
                source_size.to_f64().into(),
                output_transform.into(),
            );
            let position = Position::new(
                clamp_rect.loc.x as f64 + local.x,
                clamp_rect.loc.y as f64 + local.y,
            );

            Some(InputEvent::PointerMoved {
                position,
                delta: None,
                delta_unaccel: None,
            })
        }
        SmithayInputEvent::PointerButton { event, .. } => Some(InputEvent::PointerButton {
            button: mouse_button_from_code(event.button_code()),
            state: button_state(event.state()),
            position: pointer_pos.into(),
        }),
        SmithayInputEvent::PointerAxis { event, .. } => {
            let x_v120 = event
                .amount_v120(Axis::Horizontal)
                .map(|value| value as i32);
            let y_v120 = event.amount_v120(Axis::Vertical).map(|value| value as i32);
            let direction = if natural_scroll { -1.0 } else { 1.0 };
            let x = axis_amount(event.amount(Axis::Horizontal), x_v120) * direction;
            let y = axis_amount(event.amount(Axis::Vertical), y_v120) * direction;
            let is_finger = event.source() == AxisSource::Finger;
            let source = scroll_source(event.source());
            let delta = ScrollDelta::Axis {
                x,
                y,
                x_v120: x_v120.map(|value| if natural_scroll { -value } else { value }),
                y_v120: y_v120.map(|value| if natural_scroll { -value } else { value }),
                source,
                x_inverted: (event.relative_direction(Axis::Horizontal)
                    == AxisRelativeDirection::Inverted)
                    ^ natural_scroll,
                y_inverted: (event.relative_direction(Axis::Vertical)
                    == AxisRelativeDirection::Inverted)
                    ^ natural_scroll,
                stop_x: is_finger && event.amount(Axis::Horizontal) == Some(0.0),
                stop_y: is_finger && event.amount(Axis::Vertical) == Some(0.0),
            };

            Some(InputEvent::PointerScroll {
                delta,
                position: pointer_pos.into(),
            })
        }
        SmithayInputEvent::TouchDown { event } => Some(InputEvent::TouchDown {
            slot: event.slot().into(),
            position: absolute_touch_position(event, clamp_rect, output_transform),
            time_msec: event.time_msec(),
        }),
        SmithayInputEvent::TouchMotion { event } => Some(InputEvent::TouchMotion {
            slot: event.slot().into(),
            position: absolute_touch_position(event, clamp_rect, output_transform),
            time_msec: event.time_msec(),
        }),
        SmithayInputEvent::TouchUp { event } => Some(InputEvent::TouchUp {
            slot: event.slot().into(),
            time_msec: event.time_msec(),
        }),
        SmithayInputEvent::TouchCancel { .. } => Some(InputEvent::TouchCancel),
        SmithayInputEvent::TouchFrame { .. } => Some(InputEvent::TouchFrame),
        SmithayInputEvent::GestureSwipeBegin { event } => Some(InputEvent::GestureSwipeBegin {
            fingers: event.fingers(),
            time_msec: event.time_msec(),
        }),
        SmithayInputEvent::GestureSwipeUpdate { event } => Some(InputEvent::GestureSwipeUpdate {
            delta: event.delta().into(),
            time_msec: event.time_msec(),
        }),
        SmithayInputEvent::GestureSwipeEnd { event } => Some(InputEvent::GestureSwipeEnd {
            cancelled: event.cancelled(),
            time_msec: event.time_msec(),
        }),
        _ => None,
    }
}

/// Convert an absolute touchscreen contact into compositor-global logical
/// coordinates. Touch panels, like tablets, report the panel's unrotated
/// coordinate space, so apply the output transform before adding its origin.
fn absolute_touch_position<B, E>(
    event: &E,
    clamp_rect: Rectangle<i32, Logical>,
    output_transform: Transform,
) -> Position
where
    B: smithay::backend::input::InputBackend,
    E: smithay::backend::input::AbsolutePositionEvent<B>,
{
    let source_size = output_transform.invert().transform_size(clamp_rect.size);
    let local = transform_absolute_position(
        event.position_transformed(source_size).to_f64().into(),
        source_size.to_f64().into(),
        output_transform.into(),
    );
    Position::new(
        clamp_rect.loc.x as f64 + local.x,
        clamp_rect.loc.y as f64 + local.y,
    )
}

fn key_state(state: SmithayKeyState) -> KeyState {
    match state {
        SmithayKeyState::Pressed => KeyState::Pressed,
        SmithayKeyState::Released => KeyState::Released,
    }
}

fn button_state(state: ButtonState) -> KeyState {
    match state {
        ButtonState::Pressed => KeyState::Pressed,
        ButtonState::Released => KeyState::Released,
    }
}

fn mouse_button_from_code(code: u32) -> MouseButton {
    match code {
        0x110 => MouseButton::Left,
        0x111 => MouseButton::Right,
        0x112 => MouseButton::Middle,
        0x113 => MouseButton::Back,
        0x114 => MouseButton::Forward,
        other => MouseButton::Other(other as u16),
    }
}

fn scroll_source(source: AxisSource) -> ScrollSource {
    match source {
        AxisSource::Finger => ScrollSource::Finger,
        AxisSource::Continuous => ScrollSource::Continuous,
        AxisSource::Wheel => ScrollSource::Wheel,
        AxisSource::WheelTilt => ScrollSource::WheelTilt,
    }
}

fn axis_amount(amount: Option<f64>, v120: Option<i32>) -> f64 {
    amount.unwrap_or_else(|| {
        v120.map(|value| f64::from(value) * 15.0 / 120.0)
            .unwrap_or(0.0)
    })
}

fn clamp_pointer_position(position: Position, bounds: Bounds) -> Position {
    let min_x = bounds.origin.x;
    let min_y = bounds.origin.y;
    let max_x = min_x + bounds.size.width - f64::EPSILON;
    let max_y = min_y + bounds.size.height - f64::EPSILON;
    Position::new(
        position.x.clamp(min_x, max_x.max(min_x)),
        position.y.clamp(min_y, max_y.max(min_y)),
    )
}

fn relative_pointer_position(
    pointer_pos: Position,
    delta: Delta,
    scale_factor: f64,
    bounds: Bounds,
) -> Position {
    // Backend motion is expressed in physical pixels, while compositor pointer
    // positions use logical coordinates. Invalid or sub-1 scales retain the
    // historical 1x fallback instead of amplifying motion.
    let scale_factor = scale_factor.max(1.0);
    let logical_delta = Delta::new(delta.x / scale_factor, delta.y / scale_factor);
    clamp_pointer_position(pointer_pos + logical_delta, bounds)
}

impl From<SmithayPoint<f64, Logical>> for Position {
    fn from(point: SmithayPoint<f64, Logical>) -> Self {
        Self::new(point.x, point.y)
    }
}

impl From<Position> for SmithayPoint<f64, Logical> {
    fn from(position: Position) -> Self {
        (position.x, position.y).into()
    }
}

impl From<SmithayPoint<f64, Logical>> for Delta {
    fn from(point: SmithayPoint<f64, Logical>) -> Self {
        Self::new(point.x, point.y)
    }
}

impl From<Delta> for SmithayPoint<f64, Logical> {
    fn from(delta: Delta) -> Self {
        (delta.x, delta.y).into()
    }
}

impl From<SmithaySize<f64, Logical>> for Size {
    fn from(size: SmithaySize<f64, Logical>) -> Self {
        Self::new(size.w, size.h)
    }
}

impl From<Rectangle<i32, Logical>> for Bounds {
    fn from(bounds: Rectangle<i32, Logical>) -> Self {
        Self::new(
            Position::new(f64::from(bounds.loc.x), f64::from(bounds.loc.y)),
            Size::new(f64::from(bounds.size.w), f64::from(bounds.size.h)),
        )
    }
}

impl From<Transform> for OutputTransform {
    fn from(transform: Transform) -> Self {
        match transform {
            Transform::Normal => Self::Normal,
            Transform::_90 => Self::Rotate90,
            Transform::_180 => Self::Rotate180,
            Transform::_270 => Self::Rotate270,
            Transform::Flipped => Self::Flipped,
            Transform::Flipped90 => Self::Flipped90,
            Transform::Flipped180 => Self::Flipped180,
            Transform::Flipped270 => Self::Flipped270,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_button_codes_map_to_canonical_buttons() {
        assert_eq!(mouse_button_from_code(0x110), MouseButton::Left);
        assert_eq!(mouse_button_from_code(0x111), MouseButton::Right);
        assert_eq!(mouse_button_from_code(0x112), MouseButton::Middle);
        assert_eq!(mouse_button_from_code(0x113), MouseButton::Back);
        assert_eq!(mouse_button_from_code(0x114), MouseButton::Forward);
        assert_eq!(mouse_button_from_code(0x120), MouseButton::Other(0x120));
    }

    #[test]
    fn smithay_key_and_button_states_map_to_canonical_states() {
        assert_eq!(key_state(SmithayKeyState::Pressed), KeyState::Pressed);
        assert_eq!(key_state(SmithayKeyState::Released), KeyState::Released);
        assert_eq!(button_state(ButtonState::Pressed), KeyState::Pressed);
        assert_eq!(button_state(ButtonState::Released), KeyState::Released);
    }

    #[test]
    fn smithay_scroll_sources_map_to_canonical_sources() {
        assert_eq!(scroll_source(AxisSource::Finger), ScrollSource::Finger);
        assert_eq!(
            scroll_source(AxisSource::Continuous),
            ScrollSource::Continuous
        );
        assert_eq!(scroll_source(AxisSource::Wheel), ScrollSource::Wheel);
        assert_eq!(
            scroll_source(AxisSource::WheelTilt),
            ScrollSource::WheelTilt
        );
    }

    #[test]
    fn scroll_amount_prefers_precise_units_and_falls_back_to_v120() {
        assert_eq!(axis_amount(Some(2.5), Some(120)), 2.5);
        assert_eq!(axis_amount(None, Some(120)), 15.0);
        assert_eq!(axis_amount(None, Some(-240)), -30.0);
        assert_eq!(axis_amount(None, None), 0.0);
    }

    #[test]
    fn relative_pointer_position_is_clamped_inside_offset_output() {
        let bounds = Bounds::new(Position::new(100.0, 50.0), Size::new(800.0, 600.0));

        assert_eq!(
            clamp_pointer_position(Position::new(50.0, 800.0), bounds),
            Position::new(100.0, 650.0 - f64::EPSILON)
        );
        assert_eq!(
            clamp_pointer_position(Position::new(400.0, 300.0), bounds),
            Position::new(400.0, 300.0)
        );
    }

    #[test]
    fn relative_pointer_position_uses_logical_output_scale() {
        let bounds = Bounds::new(Position::new(100.0, 50.0), Size::new(800.0, 600.0));
        let pointer = Position::new(200.0, 150.0);
        let delta = Delta::new(40.0, -20.0);

        assert_eq!(
            relative_pointer_position(pointer, delta, 1.0, bounds),
            Position::new(240.0, 130.0)
        );
        assert_eq!(
            relative_pointer_position(pointer, delta, 2.0, bounds),
            Position::new(220.0, 140.0)
        );
    }

    #[test]
    fn scaled_relative_pointer_position_is_clamped_after_conversion() {
        let bounds = Bounds::new(Position::new(100.0, 50.0), Size::new(800.0, 600.0));

        assert_eq!(
            relative_pointer_position(
                Position::new(895.0, 55.0),
                Delta::new(20.0, -20.0),
                2.0,
                bounds,
            ),
            Position::new(900.0 - f64::EPSILON, 50.0)
        );
    }
}
