//! Shared input primitives for FocalDesk backends and the compositor engine.
//!
//! This crate owns backend-independent event and state types plus pure input
//! transforms. Routing, focus, grabs, and Wayland client delivery remain in
//! `focaldesk-engine`, where the live compositor state is available.

mod event;
mod geometry;
mod state;
mod transform;
#[cfg(feature = "smithay")]
mod translate;

pub use event::{InputEvent, KeyState, Modifiers, MouseButton, ScrollDelta, ScrollSource};
pub use geometry::{Bounds, Delta, OutputTransform, Position, Size};
pub use state::InputState;
pub use transform::transform_absolute_position;
#[cfg(feature = "smithay")]
pub use translate::translate_backend_input;
