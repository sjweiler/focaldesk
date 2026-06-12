use crate::CursorHotspot;

pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot: CursorHotspot,
    pub pixels: Vec<u8>, // RGBA8888
}
