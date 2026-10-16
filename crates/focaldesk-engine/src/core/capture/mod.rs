//! Trusted compositor-side output capture primitives.

mod broker;
mod error;
mod frame;

pub use broker::{CaptureConsumerId, MAX_CAPTURE_QUEUE_DEPTH, OutputCaptureBroker};
pub use error::CaptureError;
pub use frame::{CaptureFrame, CaptureGeometry};
