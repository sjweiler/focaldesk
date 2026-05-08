//! Data used when drawing the cursor into the compositor framebuffer (software fallback).

/// Top-left of the cursor sprite in output-buffer coordinates (typically pointer minus hotspot).
#[derive(Debug, Clone, Copy)]
pub struct SoftwareCursorDest {
    pub x: f64,
    pub y: f64,
}
