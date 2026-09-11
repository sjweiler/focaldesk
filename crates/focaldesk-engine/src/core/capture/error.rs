use std::fmt;

use super::{CaptureConsumerId, MAX_CAPTURE_QUEUE_DEPTH};

/// Errors returned by the shared output-capture broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    InvalidQueueDepth { requested: usize },
    UnknownConsumer(CaptureConsumerId),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQueueDepth { requested } => write!(
                formatter,
                "capture queue depth {requested} is outside the supported range 1..={MAX_CAPTURE_QUEUE_DEPTH}"
            ),
            Self::UnknownConsumer(consumer_id) => {
                write!(formatter, "unknown capture consumer {consumer_id:?}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}
