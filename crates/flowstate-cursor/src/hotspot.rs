#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorHotspot {
    pub x: u32,
    pub y: u32,
}

impl CursorHotspot {
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}
