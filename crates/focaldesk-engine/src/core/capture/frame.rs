use std::time::Instant;

use focaldesk_types::OutputId;
use smithay::utils::{Physical, Rectangle, Size, Transform};

/// Output geometry attached to every captured frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureGeometry {
    pub size: Size<i32, Physical>,
    pub scale: f64,
    pub transform: Transform,
}

/// A compositor-produced frame delivered to one capture consumer.
#[derive(Clone, Debug)]
pub struct CaptureFrame<B> {
    pub output_id: OutputId,
    pub serial: u64,
    pub captured_at: Instant,
    pub geometry: CaptureGeometry,
    pub buffer: B,
    pub damage: Vec<Rectangle<i32, Physical>>,
    pub full_refresh: bool,
}

impl<B> CaptureFrame<B> {
    pub(crate) fn force_full_refresh(&mut self) {
        self.damage = vec![Rectangle::from_loc_and_size((0, 0), self.geometry.size)];
        self.full_refresh = true;
    }
}
