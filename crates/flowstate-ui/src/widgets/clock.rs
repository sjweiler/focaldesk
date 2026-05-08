use chrono::{DateTime, Local, Utc};
use smithay::utils::{Point, Physical};
use smithay::backend::renderer::gles::GlesTexture;
use std::time::Instant;




use super::{WidgetCtx, WidgetRect};


pub struct ClockWidget {
    pub texture: Option<GlesTexture>,
    pub last_string: String,
    pub last_update: Instant,
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
