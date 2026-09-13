//! Renderer boundary for FocalDesk.
//!
//! This crate deliberately exposes compositor concepts instead of mirroring
//! every wgpu type. Backend-specific handles stay in their implementation so
//! the GLES renderer can later implement the same boundary without depending
//! on wgpu.

#[cfg(feature = "wgpu")]
mod wgpu_vulkan;

#[cfg(feature = "wgpu")]
pub use wgpu_vulkan::WgpuVulkanRenderer;

/// Information useful for diagnostics and backend capability decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererInfo {
    pub api: GraphicsApi,
    pub adapter_name: String,
    pub driver: String,
    pub driver_info: String,
    pub surface_format: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsApi {
    Vulkan,
}

/// Outcome of presenting one compositor frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentResult {
    Presented,
    SurfaceReconfigured,
    Skipped,
}

/// Pixel layout and transfer function for a compositor texture upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePixelFormat {
    Bgra8Srgb,
    Rgba8Srgb,
}

/// One premultiplied texture quad ready for composition.
///
/// The destination is expressed in physical output pixels. `stride` may be
/// wider than `width * 4`.
#[derive(Clone, Debug)]
pub struct TextureQuad {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: FramePixelFormat,
    pub destination: [i32; 4],
    /// Normalized texture coordinates `[left, top, right, bottom]`.
    pub source_uv: [f32; 4],
}

/// The small presentation-facing portion of a FocalDesk renderer.
///
/// Scene textures, imported DMA-BUFs, synchronization, and drawing commands
/// will be added here as those compositor paths are ported.
pub trait PresentRenderer {
    fn info(&self) -> &RendererInfo;
    fn resize(&mut self, width: u32, height: u32);
    fn present_frame(&mut self, surfaces: &[TextureQuad]) -> anyhow::Result<PresentResult>;
}
