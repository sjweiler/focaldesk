//! Trusted compositor-side output capture primitives.

mod broker;
mod frame;

pub use broker::{CaptureConsumerId, OutputCaptureBroker, MAX_CAPTURE_QUEUE_DEPTH};
pub use frame::{CaptureFrame, CaptureGeometry};
