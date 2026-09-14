//! Renderer boundary for FocalDesk.
//!
//! This crate deliberately exposes compositor concepts instead of mirroring
//! every wgpu type. Backend-specific handles stay in their implementation so
//! the GLES renderer can later implement the same boundary without depending
//! on wgpu.

use std::any::Any;
use std::fmt;
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::sync::Arc;

#[cfg(feature = "wgpu")]
mod wgpu_vulkan;

#[cfg(all(feature = "ash-drm", unix))]
mod ash_drm;

#[cfg(all(feature = "ash-drm", unix))]
pub use ash_drm::{
    AshDrmCapture, AshDrmOutputLut, AshDrmRenderer, AshDrmSubmission, AshDrmTransfer,
};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FramePixelFormat {
    Bgra8Srgb,
    Rgba8Srgb,
}

/// Transfer decoding used to move a client buffer into scene-linear light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum FrameTransferFunction {
    #[default]
    Srgb = 0,
    Linear = 1,
    Gamma22 = 2,
    St2084Pq = 3,
    ExtendedSrgb = 4,
    Bt1886 = 5,
    Hlg = 6,
}

/// Per-texture transform from decoded client RGB into the linear scene gamut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureColorTransform {
    pub transfer: FrameTransferFunction,
    pub client_to_scene: [[f32; 3]; 3],
    pub reference_white_nits: f32,
    pub linear_to_scene_scale: f32,
    pub source_bits: f32,
}

impl Default for TextureColorTransform {
    fn default() -> Self {
        Self {
            transfer: FrameTransferFunction::Srgb,
            client_to_scene: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            reference_white_nits: 80.0,
            linear_to_scene_scale: 1.0,
            source_bits: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

/// A single-plane Linux DMA-BUF which may be imported directly by a renderer.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct LinuxDmabuf {
    pub fd: Arc<OwnedFd>,
    pub modifier: u64,
    pub offset: u64,
}

/// A Linux DMA-BUF format/modifier pair accepted by the Vulkan renderer.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinuxDmabufFormat {
    pub format: FramePixelFormat,
    pub modifier: u64,
}

/// Kernel device number for the Vulkan adapter's DRM render node.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrmRenderNode {
    pub major: u32,
    pub minor: u32,
}

/// DMA-BUF target owned by the compositor's DRM/GBM swapchain.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct DrmRenderTarget {
    pub planes: Vec<Arc<OwnedFd>>,
    pub offsets: Vec<u32>,
    pub strides: Vec<u32>,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

/// Keeps compositor-owned resources alive until the GPU finishes a frame.
#[derive(Clone)]
pub struct FrameRetention {
    _resource: Arc<dyn Any + Send + Sync>,
}

impl FrameRetention {
    pub fn new(value: impl Any + Send + Sync) -> Self {
        Self {
            _resource: Arc::new(value),
        }
    }
}

impl fmt::Debug for FrameRetention {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FrameRetention")
            .finish_non_exhaustive()
    }
}

/// One premultiplied texture quad ready for composition.
///
/// The destination is expressed in physical output pixels. `stride` may be
/// wider than `width * 4`.
#[derive(Clone, Debug)]
pub struct TextureQuad {
    /// Stable content identity. Reusing it allows the renderer to retain the GPU texture.
    pub cache_key: u64,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: FramePixelFormat,
    /// Decode and gamut transform for this texture. Shell assets use sRGB.
    pub color_transform: TextureColorTransform,
    /// Optional zero-copy source. `pixels` remains a synchronized fallback for
    /// devices or format/modifier combinations that reject external import.
    #[cfg(unix)]
    pub dmabuf: Option<LinuxDmabuf>,
    /// Changed buffer-coordinate rectangles. Empty means the cached pixels are unchanged.
    pub damage: Vec<[u32; 4]>,
    pub destination: [i32; 4],
    /// Normalized texture coordinates `[left, top, right, bottom]`.
    pub source_uv: [f32; 4],
    pub transform: FrameTransform,
    /// Premultiplied tint multiplied into sampled pixels.
    pub tint: [f32; 4],
    /// Resource lifetime token released only after this frame's GPU work completes.
    pub retention: Option<FrameRetention>,
}

/// A premultiplied-color rectangle in physical output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolidQuad {
    pub destination: [i32; 4],
    pub color: [f32; 4],
    pub corner_radius: f32,
}

/// One vertex in a compositor-owned textured triangle mesh. Positions are in
/// physical output pixels and colors are premultiplied sRGBA.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

/// Indexed triangles referencing a texture already supplied for this frame.
#[derive(Clone, Debug, Default)]
pub struct TexturedMesh {
    pub texture_key: u64,
    pub clip_rect: [i32; 4],
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

/// The small presentation-facing portion of a FocalDesk renderer.
///
/// Shell scene textures and higher-level drawing commands will be added here as
/// those compositor paths are ported.
pub trait PresentRenderer {
    fn info(&self) -> &RendererInfo;
    fn resize(&mut self, width: u32, height: u32);
    fn present_frame(
        &mut self,
        background: &[SolidQuad],
        surfaces: &[TextureQuad],
        overlay: &[SolidQuad],
        overlay_after_surface: usize,
        damage: &[[i32; 4]],
    ) -> anyhow::Result<PresentResult>;
}
