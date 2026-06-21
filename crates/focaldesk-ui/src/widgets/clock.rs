use smithay::backend::renderer::gles::GlesTexture;
use std::time::Instant;

pub struct ClockWidget {
    pub texture: Option<GlesTexture>,
    pub last_string: String,
    pub last_update: Instant,
}

impl Default for ClockWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockWidget {
    pub fn new() -> Self {
        Self {
            texture: None,
            last_string: String::new(),
            last_update: Instant::now(),
        }
    }
}
