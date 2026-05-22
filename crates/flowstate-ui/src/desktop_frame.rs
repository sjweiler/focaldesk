use std::time::Instant;

use flowstate_types::OutputId;
use smithay::utils::{Logical, Physical, Rectangle, Scale};

/// Per-frame output context for desktop chrome / UI rendering.
#[derive(Clone, Debug)]
pub struct DesktopFrameCtx {
    pub output_size: (i32, i32),
    pub output_scale: Scale<f64>,
    pub work: Rectangle<i32, Logical>,
    pub active_output: OutputId,
    pub rendering_output: OutputId,
    pub now: Instant,
    pub start_time: Instant,
}

impl DesktopFrameCtx {
    pub fn fullscreen_damage(&self) -> Vec<Rectangle<i32, Physical>> {
        vec![Rectangle::from_loc_and_size(
            (0, 0),
            (self.output_size.0, self.output_size.1),
        )]
    }
}
