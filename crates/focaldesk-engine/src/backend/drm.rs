// DRM/KMS backend — uses the same [`DesktopState`] path as winit via [`crate::backend::common::NestedDesktop`].
// Full session/udev/scanout should follow the Smithay anvil `udev` backend pattern.

use crate::backend::common::{
    bootstrap_compositor_core, physical_size_mm_from_pixels, refresh_portal_services,
};
use crate::backend::drm::drm::buffer::DrmModifier;
use drm::control::{atomic::AtomicModeReq, connector, crtc, property};
use drm_sys as drm_ffi;
use smithay::backend::allocator::Allocator;
use smithay::backend::input::{InputEvent, KeyState};
use smithay::backend::renderer::gles::GlesError;
use smithay::backend::renderer::Offscreen;
use smithay::reexports::drm::control::Device as _;
// `DrmOutput::render_frame` / `initialize_output` drive an internal [`smithay::backend::drm::compositor::DrmCompositor`].

use smithay::backend::input::KeyboardKeyEvent;
//use smithay::backend::renderer::element::{Id, Kind};
use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, prepare_output,
};
use smithay::backend::renderer::utils::DamageBag;
use smithay::backend::renderer::Frame;
//use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    render_elements, texture::TextureRenderElement, Id, Kind,
};

use focaldesk_flow::keybinds::BackendKind;

// DRM/KMS backend for FocalDesk.
//
// This is the real hardware backend counterpart to the winit backend.
// It keeps compositor state shared, but owns its own session/device/input/output plumbing.
//
// This is intentionally a bring-up skeleton:
// - struct layout is real
// - event loop wiring is real
// - device/output attach points are real
// - many internals are still TODO so you can connect them to your existing FocalDesk code

use crate::core::backend_render::draw_output;
use anyhow::{anyhow, Context, Result};
use smithay::backend::renderer::Bind;
use smithay::utils::DeviceFd;

use std::{
    collections::HashMap,
    error::Error,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::backend::common::translate_backend_input;
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use focaldesk_logging::flog;
use focaldesk_resources::RenderResources;
use focaldesk_types::OutputId;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::{Renderer, Texture as SmithayTexture};

use smithay::{
    backend::{
        allocator::{gbm::GbmAllocator, gbm::GbmBufferFlags, Fourcc, Modifier},
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode},
        egl::{self, context::ContextPriority, EGLContext},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::solid::SolidColorRenderElement,
            gles::{GlesRenderer, GlesTarget, GlesTexture, Uniform},
            Color32F, ExportMem, ImportDma, ImportEgl,
        },
        session::{libseat::LibSeatSession, Session},
        udev::{primary_gpu, UdevBackend},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop,
        gbm::Device as GbmDevice,
        input::Libinput,
        wayland_server::{Client, Display, ListeningSocket},
    },
    utils::{Buffer, IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use smithay::backend::drm::{
    compositor::FrameFlags,
    exporter::gbm::GbmFramebufferExporter,
    output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
};

use crate::core::chrome_layout::build_chrome_layout;
use crate::core::{
    desktop::{DamageSource, DesktopState},
    ui_state::UiState,
    OutputState, SceneState,
};

use smithay::backend::egl::{EGLDevice, EGLDisplay};

use smithay::reexports::drm;

use smithay::reexports::rustix::fs::OFlags;

use chrono::Local;
use image::{ImageBuffer, Rgba};
use std::fs;

#[cfg(feature = "xwayland")]
use crate::backend::common::{finish_xwayland_startup, start_xwayland};
use drm::control::Mode;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayConfig {
    pub name: String,
    pub enabled: bool,

    pub mode_width: i32,
    pub mode_height: i32,
    pub refresh_mhz: i32,

    pub scale: f64,

    pub logical_x: i32,
    pub logical_y: i32,

    pub physical_width_mm: Option<i32>,
    pub physical_height_mm: Option<i32>,

    pub primary: bool,
    pub transform: DisplayTransform,

    #[serde(default)]
    pub hdr_supported: bool,
    #[serde(default)]
    pub hdr_requested: bool,
    #[serde(default)]
    pub hdr_enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct HdrSupport {
    pub has_hdr_metadata_property: bool,
    pub has_bt2020_colorspace: bool,
    pub colorspaces: Vec<String>,
    pub current_colorspace: Option<String>,
    pub max_bpc: Option<HdrBpcRange>,
    pub current_max_bpc: Option<u64>,
    pub hdr_metadata_blob: Option<u64>,
    pub edid_hdr_static_metadata: bool,
    pub edid_pq: bool,
    pub edid_hlg: bool,
}

#[derive(Debug, Clone)]
pub struct HdrBpcRange {
    pub min: u64,
    pub max: u64,
}

impl HdrSupport {
    fn has_connector_controls(&self) -> bool {
        self.has_hdr_metadata_property || self.has_bt2020_colorspace || self.max_bpc.is_some()
    }

    fn can_enable(&self) -> bool {
        self.has_hdr_metadata_property
            && self.has_bt2020_colorspace
            && self.edid_hdr_static_metadata
            && self.edid_pq
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DisplayTransform {
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

render_elements! {
    pub DrmPresentElement<=GlesRenderer>;
    Texture=TextureRenderElement<GlesTexture>,
}

pub struct OffscreenOutput {
    pub size: Size<i32, Physical>,
    pub texture: GlesTexture,
}

pub struct DrmRuntime {
    pub session: LibSeatSession,
    pub primary_gpu: DrmNode,
    pub devices: HashMap<DrmNode, DrmGpu>,
    pub should_stop: bool,
}

pub struct DrmGpu {
    pub node: DrmNode,
    pub fd: DrmDeviceFd,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub registration_token: RegistrationToken,

    pub render_node: Option<DrmNode>,

    pub allocator: GbmAllocator<DrmDeviceFd>,
    pub framebuffer_exporter: GbmFramebufferExporter<DrmDeviceFd>,
    pub drm_output_manager: DrmOutputManager<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,

    pub outputs: HashMap<crtc::Handle, DrmOutputState>,
}

pub struct DrmOutputState {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: Mode,
    pub output: Output,
    pub pending_modeset: bool,
}

/// Shared compositor core, without any synthetic nested-output assumptions.
pub(crate) struct CompositorCore {
    pub display: Display<DesktopState>,
    #[cfg(feature = "xwayland")]
    pub xwayland_event_loop: EventLoop<'static, DesktopState>,
    pub listener: ListeningSocket,
    pub wayland_display: String,
    pub state: DesktopState,
    pub clients: Vec<Client>,
    pub ui_state: UiState<GlesTexture>,
    pub scene: SceneState,
    pub output_state: OutputState,
    pub resources: RenderResources,
    pub start: Instant,
    pub last_now: Instant,
}

/// Per-CRTC/output state.
pub struct DrmSurfaceState {
    pub connector: connector::Handle,
    pub output: Output,
    pub mode: WlMode,
    pub size: Size<i32, Physical>,
    pub output_id: OutputId,
    pub origin: Point<i32, Logical>,
    pub present_render_id: Id,
    pub present_damage: DamageBag<i32, Buffer>,
    pub hdr_support: HdrSupport,
    pub hdr_render_supported: bool,
    pub hdr_offscreen_format: Option<Fourcc>,
    pub hdr_metadata_blob: Option<u64>,
    pub hdr_enabled_applied: bool,

    pub drm_output: DrmOutput<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        Option<OutputPresentationFeedback>,
        DrmDeviceFd,
    >,
    pub offscreen: Option<OffscreenOutput>,
    pub sdr_offscreen: Option<OffscreenOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectorHdrApplyResult {
    Unchanged,
    Applied,
    Rejected,
}

const SDR_OFFSCREEN_FORMAT: Fourcc = Fourcc::Abgr8888;
const HDR_OFFSCREEN_FORMATS: [Fourcc; 2] = [Fourcc::Abgr2101010, Fourcc::Argb2101010];
const HDR_SCANOUT_FORMATS: [Fourcc; 2] = [Fourcc::Argb2101010, Fourcc::Xrgb2101010];

type FlowDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

/// Per-DRM-device backend state.
pub struct DrmDeviceState {
    pub registration_token: RegistrationToken,
    pub render_node: Option<DrmNode>,
    pub renderer: GlesRenderer,
    pub drm_output_manager: FlowDrmOutputManager,
    pub surfaces: HashMap<drm::control::crtc::Handle, DrmSurfaceState>,
}

/// Whole backend state for tty/udev/libinput/drm.
pub struct DrmBackend {
    pub session: LibSeatSession,
    pub devices: HashMap<DrmNode, DrmDeviceState>,
}

/// Loop data for the DRM backend.
pub(crate) struct DrmLoopData {
    pub core: CompositorCore,
    pub backend: DrmBackend,
    pub libinput: Libinput,
    pub should_stop: bool,
}

fn display_config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });

    base.join("focaldesk").join("displays.json")
}

fn load_display_config() -> Vec<DisplayConfig> {
    let path = display_config_path();

    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(displays) => displays,
            Err(err) => {
                flog(&format!(
                    "Failed to parse display config {}; using defaults: {}",
                    path.display(),
                    err
                ));
                Vec::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            flog(&format!(
                "Failed to read display config {}; using defaults: {}",
                path.display(),
                err
            ));
            Vec::new()
        }
    }
}

fn configured_display_scale(displays: &[DisplayConfig], name: &str) -> f64 {
    const MIN_SCALE: f64 = 1.0;
    const MAX_SCALE: f64 = 4.0;

    let Some(scale) = displays
        .iter()
        .find(|display| display.name == name)
        .map(|display| display.scale)
    else {
        return MIN_SCALE;
    };

    if !scale.is_finite() || scale < MIN_SCALE {
        flog(&format!(
            "Ignoring invalid display scale for {}: {}; using {}",
            name, scale, MIN_SCALE
        ));
        return MIN_SCALE;
    }

    if scale > MAX_SCALE {
        flog(&format!(
            "Clamping display scale for {} from {} to {}",
            name, scale, MAX_SCALE
        ));
        return MAX_SCALE;
    }

    scale
}

fn configured_display_hdr_enabled(displays: &[DisplayConfig], name: &str) -> bool {
    displays
        .iter()
        .find(|display| display.name == name)
        .map(|display| display.hdr_requested || display.hdr_enabled)
        .unwrap_or(false)
}

fn write_display_config(displays: &[DisplayConfig]) -> Result<()> {
    let path = display_config_path();
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("display config path has no parent: {}", path.display()))?;

    std::fs::create_dir_all(&dir)?;

    let json = serde_json::to_string_pretty(displays)?;
    std::fs::write(&path, json)?;

    flog(&format!("Wrote display config to {}", path.display()));

    Ok(())
}

/// `Fourcc::Argb8888` / `GL_BGRA8_EXT` readback: GL stores bottom row first; convert to top-down RGBA for PNG.
fn bgra_gl_bottom_left_to_png_rgba(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    debug_assert_eq!(src.len(), width * height * 4);
    let stride = width * 4;
    let mut out = vec![0u8; src.len()];
    for y in 0..height {
        let src_row = y * stride;
        let dst_row = (height - 1 - y) * stride;
        for x in 0..width {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            out[d] = src[s + 2];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s];
            out[d + 3] = src[s + 3];
        }
    }
    out
}

/// Read pixels from the bound GLES framebuffer (same FBO the frame was rendered to).
///
/// DRM offscreen uses [`Fourcc::Abgr8888`] (`GL_RGBA8` + `GL_RGBA` read), which tends to be
/// reliable on GLES. `Fourcc::Argb8888` uses `GL_BGRA_EXT` readback; some stacks return zeros
/// from FBO read despite a valid draw.
fn copy_framebuffer_target_to_png_rgba(
    renderer: &mut GlesRenderer,
    target: &GlesTarget<'_>,
    width: i32,
    height: i32,
) -> Result<Vec<u8>> {
    use smithay::backend::renderer::gles::ffi;

    let w = width as usize;
    let h = height as usize;

    renderer
        .with_context(|gl| unsafe {
            gl.BindBuffer(ffi::PIXEL_PACK_BUFFER, 0);
        })
        .map_err(|e| anyhow!("screenshot GL state: {e}"))?;

    let region = Rectangle::<i32, Buffer>::from_loc_and_size(
        Point::from((0, 0)),
        Size::from((width, height)),
    );

    match renderer.copy_framebuffer(target, region, Fourcc::Abgr8888) {
        Ok(mapping) => {
            renderer
                .with_context(|gl| unsafe {
                    gl.Finish();
                })
                .map_err(|e| anyhow!("screenshot Finish: {e}"))?;
            let src = renderer
                .map_texture(&mapping)
                .map_err(|e| anyhow!("map_texture (ABGR8888): {e}"))?;
            Ok(src.to_vec())
        }
        Err(e1) => {
            flog(&format!(
                "screenshot: ABGR8888 read failed ({e1}), trying ARGB8888/BGRA"
            ));
            let mapping = renderer
                .copy_framebuffer(target, region, Fourcc::Argb8888)
                .map_err(|e2| anyhow!("copy_framebuffer ARGB8888 after ABGR fail: {e2}"))?;
            renderer
                .with_context(|gl| unsafe {
                    gl.Finish();
                })
                .map_err(|e| anyhow!("screenshot Finish: {e}"))?;
            let src = renderer
                .map_texture(&mapping)
                .map_err(|e| anyhow!("map_texture (ARGB8888): {e}"))?;
            Ok(bgra_gl_bottom_left_to_png_rgba(src, w, h))
        }
    }
}

fn capture_surface_pixels(
    renderer: &mut GlesRenderer,
    surface: &mut DrmSurfaceState,
) -> Result<Vec<u8>> {
    let offscreen = surface
        .sdr_offscreen
        .as_mut()
        .filter(|_| surface.hdr_enabled_applied)
        .or(surface.offscreen.as_mut())
        .ok_or_else(|| anyhow!("offscreen texture missing for capture"))?;

    let target = renderer
        .bind(&mut offscreen.texture)
        .map_err(|e| anyhow!("bind offscreen for capture: {e}"))?;

    copy_framebuffer_target_to_png_rgba(renderer, &target, offscreen.size.w, offscreen.size.h)
}

fn present_source_texture(surface: &DrmSurfaceState) -> Option<&GlesTexture> {
    surface
        .offscreen
        .as_ref()
        .map(|offscreen| &offscreen.texture)
}

fn capture_source_texture(surface: &DrmSurfaceState) -> Option<&GlesTexture> {
    surface
        .sdr_offscreen
        .as_ref()
        .filter(|_| surface.hdr_enabled_applied)
        .or(surface.offscreen.as_ref())
        .map(|offscreen| &offscreen.texture)
}

fn blit_rgba(
    dst: &mut [u8],
    dst_width: usize,
    dst_height: usize,
    src: &[u8],
    src_width: usize,
    src_height: usize,
    dst_x: usize,
    dst_y: usize,
) -> Result<()> {
    if dst_x + src_width > dst_width || dst_y + src_height > dst_height {
        return Err(anyhow!("blit out of bounds"));
    }

    let dst_stride = dst_width * 4;
    let src_stride = src_width * 4;

    for row in 0..src_height {
        let src_start = row * src_stride;
        let src_end = src_start + src_stride;

        let dst_start = (dst_y + row) * dst_stride + dst_x * 4;
        let dst_end = dst_start + src_stride;

        dst[dst_start..dst_end].copy_from_slice(&src[src_start..src_end]);
    }

    Ok(())
}

fn save_all_outputs_screenshot(
    renderer: &mut GlesRenderer,
    surfaces: &mut HashMap<drm::control::crtc::Handle, DrmSurfaceState>,
    seq: u64,
) -> Result<PathBuf> {
    if surfaces.is_empty() {
        return Err(anyhow!("no DRM surfaces available for screenshot"));
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for surface in surfaces.values() {
        min_x = min_x.min(surface.origin.x);
        min_y = min_y.min(surface.origin.y);
        max_x = max_x.max(surface.origin.x + surface.size.w);
        max_y = max_y.max(surface.origin.y + surface.size.h);
    }

    let total_width = (max_x - min_x) as usize;
    let total_height = (max_y - min_y) as usize;

    let mut desktop_pixels = vec![0u8; total_width * total_height * 4];

    for surface in surfaces.values_mut() {
        let pixels = capture_surface_pixels(renderer, surface)?;
        let width = surface.size.w as usize;
        let height = surface.size.h as usize;

        let dst_x = (surface.origin.x - min_x) as usize;
        let dst_y = (surface.origin.y - min_y) as usize;

        blit_rgba(
            &mut desktop_pixels,
            total_width,
            total_height,
            &pixels,
            width,
            height,
            dst_x,
            dst_y,
        )?;
    }

    save_rgba_png(
        total_width as i32,
        total_height as i32,
        desktop_pixels,
        "all-outputs",
        seq,
    )
}

fn flip_rgba_horizontal(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let stride = width * 4;
    let mut out = vec![0u8; data.len()];

    for y in 0..height {
        for x in 0..width {
            let src = y * stride + x * 4;
            let dst = y * stride + (width - 1 - x) * 4;
            out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
        }
    }

    out
}

fn flip_rgba_vertical(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let stride = width * 4;
    let mut out = vec![0u8; data.len()];

    for y in 0..height {
        let src = y * stride;
        let dst = (height - 1 - y) * stride;
        out[dst..dst + stride].copy_from_slice(&data[src..src + stride]);
    }

    out
}

fn save_rgba_png(
    width: i32,
    height: i32,
    pixels: Vec<u8>,
    output_name: &str,
    seq: u64,
) -> Result<PathBuf> {
    use chrono::Local;
    use image::{ImageBuffer, Rgba};
    use std::fs;
    use std::path::PathBuf;

    let screenshot_dir =
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
            .join("Pictures")
            .join("Screenshots");

    fs::create_dir_all(&screenshot_dir)?;

    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width as u32, height as u32, pixels)
        .ok_or_else(|| anyhow!("failed to construct image buffer from screenshot bytes"))?;

    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let filename = format!("focaldesk-{}-{}-{}.png", output_name, timestamp, seq);
    let path = screenshot_dir.join(filename);

    image.save(&path)?;
    flog(&format!("Screenshot saved to {}", path.display()));
    Ok(path)
}

fn save_offscreen_screenshot(
    renderer: &mut GlesRenderer,
    surface: &mut DrmSurfaceState,
    output_name: &str,
    seq: u64,
) -> Result<PathBuf> {
    let size = surface.size;

    let pixels = capture_surface_pixels(renderer, surface)?;

    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(size.w as u32, size.h as u32, pixels)
        .ok_or_else(|| anyhow!("failed to construct image buffer from screenshot bytes"))?;

    let screenshot_dir =
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
            .join("Pictures")
            .join("Screenshots");

    fs::create_dir_all(&screenshot_dir)?;

    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let filename = format!("focaldesk-{}-{}-{}.png", output_name, timestamp, seq);
    let path = screenshot_dir.join(filename);

    image.save(&path)?;
    Ok(path)
}

/// Resolve which DRM node is primary for this seat (KMS node, matches udev `device_list` entries).
///
/// This must **not** open the device: the udev `device_added` path opens it once through the session.

fn ensure_offscreen_texture(
    renderer: &mut GlesRenderer,
    offscreen: &mut Option<OffscreenOutput>,
    size: Size<i32, Physical>,
    format: Fourcc,
) -> Result<(), GlesError> {
    let recreate = match offscreen {
        Some(existing) => existing.size != size || existing.texture.format() != Some(format),
        None => true,
    };

    if recreate {
        let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
        let texture = renderer.create_buffer(format, tex_size)?;
        *offscreen = Some(OffscreenOutput { size, texture });
    }

    Ok(())
}

fn select_hdr_offscreen_format(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Option<Fourcc> {
    let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
    HDR_OFFSCREEN_FORMATS.iter().copied().find(|format| {
        <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, *format, tex_size).is_ok()
    })
}

fn probe_hdr_scanout_format(
    allocator: &mut GbmAllocator<DrmDeviceFd>,
    size: Size<i32, Physical>,
) -> Option<Fourcc> {
    HDR_SCANOUT_FORMATS.iter().copied().find(|format| {
        allocator
            .create_buffer(
                size.w as u32,
                size.h as u32,
                *format,
                &[DrmModifier::Linear],
            )
            .is_ok()
    })
}

fn convert_sdr_scene_to_hdr(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &smithay::backend::renderer::gles::GlesTexProgram,
) -> Result<(), anyhow::Error> {
    const SDR_WHITE_NITS: f32 = 200.0;
    const HDR_MAX_NITS: f32 = 1000.0;

    let mut target = renderer
        .bind(dst)
        .map_err(|err| anyhow!("bind HDR offscreen for SDR-to-HDR conversion: {err}"))?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .map_err(|err| anyhow!("begin SDR-to-HDR conversion frame: {err}"))?;
    let src_rect =
        Rectangle::<f64, Buffer>::from_loc_and_size((0.0, 0.0), (size.w as f64, size.h as f64));
    let dst_rect = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            src,
            src_rect,
            dst_rect,
            &damage,
            &damage,
            Transform::Normal,
            1.0,
            Some(shader),
            &[
                Uniform::new("u_sdr_white_nits", SDR_WHITE_NITS),
                Uniform::new("u_max_nits", HDR_MAX_NITS),
            ],
        )
        .map_err(|err| anyhow!("render SDR-to-HDR conversion: {err}"))?;
    let _sync = frame
        .finish()
        .map_err(|err| anyhow!("finish SDR-to-HDR conversion: {err}"))?;
    Ok(())
}

pub(crate) fn collect_display_configs(
    device: &DrmDeviceState,
    core: &CompositorCore,
) -> Vec<DisplayConfig> {
    let mut displays = Vec::new();

    for (_crtc, surface) in &device.surfaces {
        let output_id = surface.output_id;

        let core_output = core.state.outputs.get(&output_id);

        let (scale, logical_x, logical_y, primary) = if let Some(o) = core_output {
            (
                o.scale_factor,
                o.logical_origin.x,
                o.logical_origin.y,
                core.state.primary_output == output_id,
            )
        } else {
            (1.0, 0, 0, false)
        };

        let w = surface.mode.size.w;
        let h = surface.mode.size.h;
        let hdr_supported = surface.hdr_support.can_enable() && surface.hdr_render_supported;
        let hdr_requested = core_output
            .map(|output| output.hdr_requested)
            .unwrap_or(false);
        let hdr_enabled = core_output
            .map(|output| output.hdr_enabled && hdr_supported)
            .unwrap_or(false);

        displays.push(DisplayConfig {
            name: surface.output.name(),
            enabled: true,

            mode_width: w,
            mode_height: h,
            refresh_mhz: surface.mode.refresh,

            scale,

            logical_x,
            logical_y,

            physical_width_mm: None, // we’ll fix this next
            physical_height_mm: None,

            primary,

            transform: DisplayTransform::Normal,

            hdr_supported,
            hdr_requested,
            hdr_enabled,
        });
    }

    displays
}

fn primary_drm_node<S: Session>(session: &S) -> Result<DrmNode>
where
    S: Session,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let primary_path = primary_gpu(session.seat())?
        .ok_or_else(|| anyhow!("No primary GPU found for seat {}", session.seat()))?;

    DrmNode::from_path(&primary_path)
        .map_err(|e| anyhow!("Failed to create DrmNode from {:?}: {e}", primary_path))
}

#[derive(Debug, Clone)]
struct EdidMonitorIdentity {
    make: String,
    model: String,
    serial_number: String,
}

fn connector_edid(
    device: &impl drm::control::Device,
    connector: connector::Handle,
) -> Option<Vec<u8>> {
    let props = device.get_properties(connector).ok()?;
    for (prop, raw_value) in props.iter() {
        let info = device.get_property(*prop).ok()?;
        if info.name().to_bytes() == b"EDID" && *raw_value != 0 {
            return device.get_property_blob(*raw_value).ok();
        }
    }
    None
}

fn connector_hdr_support(
    device: &impl drm::control::Device,
    connector: connector::Handle,
    edid: Option<&[u8]>,
) -> HdrSupport {
    let mut support = edid
        .map(parse_edid_hdr_support)
        .unwrap_or_else(HdrSupport::default);

    let Ok(props) = device.get_properties(connector) else {
        return support;
    };

    for (prop, raw_value) in props.iter() {
        let Ok(info) = device.get_property(*prop) else {
            continue;
        };
        let name = info.name().to_string_lossy();

        match name.as_ref() {
            "HDR_OUTPUT_METADATA" => {
                support.has_hdr_metadata_property = true;
                support.hdr_metadata_blob = (*raw_value != 0).then_some(*raw_value);
            }
            "Colorspace" => {
                if let drm::control::property::ValueType::Enum(values) = info.value_type() {
                    let (_, enums) = values.values();
                    support.colorspaces = enums
                        .iter()
                        .map(|value| value.name().to_string_lossy().into_owned())
                        .collect();
                    support.has_bt2020_colorspace = support
                        .colorspaces
                        .iter()
                        .any(|colorspace| colorspace.contains("BT2020"));
                    support.current_colorspace = values
                        .get_value_from_raw_value(*raw_value)
                        .map(|value| value.name().to_string_lossy().into_owned());
                }
            }
            "max bpc" => {
                if let drm::control::property::ValueType::UnsignedRange(min, max) =
                    info.value_type()
                {
                    support.max_bpc = Some(HdrBpcRange { min, max });
                    support.current_max_bpc = Some(*raw_value);
                }
            }
            _ => {}
        }
    }

    support
}

fn parse_edid_hdr_support(edid: &[u8]) -> HdrSupport {
    let mut support = HdrSupport::default();
    if edid.len() < 128
        || edid.get(0..8) != Some([0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0].as_slice())
    {
        return support;
    }

    let extension_count = edid[126] as usize;
    for ext_index in 0..extension_count {
        let start = 128 * (ext_index + 1);
        let Some(extension) = edid.get(start..start + 128) else {
            break;
        };
        if extension[0] != 0x02 {
            continue;
        }

        let dtd_offset = match extension[2] {
            0 => 127,
            offset @ 4..=127 => offset as usize,
            _ => continue,
        };
        let mut index = 4;
        while index < dtd_offset {
            let header = extension[index];
            index += 1;

            let tag = header >> 5;
            let len = (header & 0x1f) as usize;
            if len == 0 || index + len > dtd_offset {
                index += len;
                continue;
            }

            let block = &extension[index..index + len];
            index += len;

            if tag != 0x07 || block.first() != Some(&0x06) || block.len() < 3 {
                continue;
            }

            let eotf_flags = block[1];
            support.edid_hdr_static_metadata = true;
            support.edid_pq = eotf_flags & (1 << 2) != 0;
            support.edid_hlg = eotf_flags & (1 << 3) != 0;
        }
    }

    support
}

fn log_hdr_support(output_name: &str, support: &HdrSupport) {
    let max_bpc = support
        .max_bpc
        .as_ref()
        .map(|range| format!("{}..{}", range.min, range.max))
        .unwrap_or_else(|| "none".to_string());
    let current_bpc = support
        .current_max_bpc
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let current_colorspace = support.current_colorspace.as_deref().unwrap_or("unknown");

    flog(&format!(
        "HDR support: output={} metadata_property={} metadata_blob={:?} bt2020_colorspace={} colorspaces={:?} current_colorspace={} max_bpc={} current_max_bpc={} edid_hdr_static_metadata={} edid_pq={} edid_hlg={}",
        output_name,
        support.has_hdr_metadata_property,
        support.hdr_metadata_blob,
        support.has_bt2020_colorspace,
        support.colorspaces,
        current_colorspace,
        max_bpc,
        current_bpc,
        support.edid_hdr_static_metadata,
        support.edid_pq,
        support.edid_hlg,
    ));
}

fn hdr_point(x: u16, y: u16) -> drm_ffi::hdr_metadata_infoframe__bindgen_ty_1 {
    drm_ffi::hdr_metadata_infoframe__bindgen_ty_1 { x, y }
}

fn hdr_white_point(x: u16, y: u16) -> drm_ffi::hdr_metadata_infoframe__bindgen_ty_2 {
    drm_ffi::hdr_metadata_infoframe__bindgen_ty_2 { x, y }
}

fn build_hdr_output_metadata(_support: &HdrSupport) -> drm_ffi::hdr_output_metadata {
    const DRM_MODE_HDR_METADATA_TYPE1: u32 = 0;
    const HDMI_EOTF_SMPTE_ST2084: u8 = 2;
    const HDMI_STATIC_METADATA_TYPE1: u8 = 0;

    let infoframe = drm_ffi::hdr_metadata_infoframe {
        eotf: HDMI_EOTF_SMPTE_ST2084,
        metadata_type: HDMI_STATIC_METADATA_TYPE1,
        display_primaries: [
            hdr_point(34_000, 16_000),
            hdr_point(13_250, 34_500),
            hdr_point(7_500, 3_000),
        ],
        white_point: hdr_white_point(15_635, 16_450),
        max_display_mastering_luminance: 1_000,
        min_display_mastering_luminance: 50,
        max_cll: 1_000,
        max_fall: 400,
    };

    drm_ffi::hdr_output_metadata {
        metadata_type: DRM_MODE_HDR_METADATA_TYPE1,
        __bindgen_anon_1: drm_ffi::hdr_output_metadata__bindgen_ty_1 {
            hdmi_metadata_type1: infoframe,
        },
    }
}

fn create_hdr_metadata_blob(
    device: &impl drm::control::Device,
    support: &HdrSupport,
) -> Result<u64, anyhow::Error> {
    if !support.can_enable() {
        return Err(anyhow!(
            "HDR metadata requested without complete HDR support"
        ));
    }

    match device
        .create_property_blob(&build_hdr_output_metadata(support))
        .map_err(|err| anyhow!("failed to create HDR metadata blob: {err}"))?
    {
        property::Value::Blob(blob) => Ok(blob),
        other => Err(anyhow!(
            "DRM returned non-blob value for HDR metadata: {other:?}"
        )),
    }
}

fn destroy_hdr_metadata_blob(device: &impl drm::control::Device, blob: Option<u64>) {
    if let Some(blob) = blob {
        if let Err(err) = device.destroy_property_blob(blob) {
            flog(&format!(
                "Failed to destroy HDR metadata blob {blob}: {err}"
            ));
        }
    }
}

fn ensure_hdr_metadata_blob(
    device: &impl drm::control::Device,
    support: &HdrSupport,
    blob: &mut Option<u64>,
) -> Result<u64, anyhow::Error> {
    if let Some(blob) = *blob {
        return Ok(blob);
    }

    let created = create_hdr_metadata_blob(device, support)?;
    *blob = Some(created);
    Ok(created)
}

fn select_colorspace_value(
    info: &property::Info,
    _support: &HdrSupport,
    hdr_enabled: bool,
) -> Option<u64> {
    let property::ValueType::Enum(values) = info.value_type() else {
        return None;
    };

    let (_, enums) = values.values();
    let selected = if hdr_enabled {
        enums
            .iter()
            .find(|value| value.name().to_string_lossy().contains("BT2020_RGB"))
            .or_else(|| {
                enums
                    .iter()
                    .find(|value| value.name().to_string_lossy().contains("BT2020"))
            })
    } else {
        enums
            .iter()
            .find(|value| value.name().to_string_lossy() == "Default")
            .or_else(|| {
                enums
                    .iter()
                    .find(|value| value.name().to_string_lossy().contains("BT709"))
            })
            .or_else(|| {
                enums.iter().find(|value| {
                    let name = value.name().to_string_lossy();
                    name.contains("RGB") && !name.contains("BT2020")
                })
            })
            .or_else(|| enums.first())
    };

    selected.map(|value| value.value())
}

fn build_connector_hdr_req(
    device: &impl drm::control::Device,
    connector: connector::Handle,
    support: &HdrSupport,
    hdr_enabled: bool,
    hdr_metadata_blob: Option<u64>,
) -> Result<Option<AtomicModeReq>, anyhow::Error> {
    if hdr_enabled && (!support.can_enable() || hdr_metadata_blob.is_none()) {
        return Ok(None);
    }

    if !hdr_enabled && !support.has_connector_controls() {
        return Ok(None);
    }

    let props = device
        .get_properties(connector)
        .map_err(|err| anyhow!("failed to read connector properties for HDR: {err}"))?;

    let mut req = AtomicModeReq::new();
    let mut changed = false;

    for (prop, raw_value) in props.iter() {
        let info = match device.get_property(*prop) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let name = info.name().to_string_lossy();

        match name.as_ref() {
            "Colorspace" => {
                if let Some(value) = select_colorspace_value(&info, support, hdr_enabled) {
                    req.add_raw_property(connector.into(), *prop, value);
                    changed = true;
                }
            }
            "max bpc" => {
                if let property::ValueType::UnsignedRange(min, max) = info.value_type() {
                    let target = if hdr_enabled {
                        Some(max)
                    } else {
                        Some(8.clamp(min, max))
                    };
                    if let Some(target) = target {
                        req.add_property(connector, *prop, property::Value::UnsignedRange(target));
                        changed = true;
                    }
                }
            }
            "HDR_OUTPUT_METADATA" => {
                if let property::ValueType::Blob = info.value_type() {
                    let target = if hdr_enabled {
                        hdr_metadata_blob.unwrap_or(*raw_value)
                    } else {
                        0
                    };
                    req.add_property(connector, *prop, property::Value::Blob(target));
                    changed = true;
                }
            }
            _ => {}
        }
    }

    if !changed {
        return Ok(None);
    }

    Ok(Some(req))
}

fn test_connector_hdr_state(
    device: &impl drm::control::Device,
    connector: connector::Handle,
    support: &HdrSupport,
    hdr_enabled: bool,
    hdr_metadata_blob: Option<u64>,
) -> Result<bool, anyhow::Error> {
    if hdr_enabled && (!support.can_enable() || hdr_metadata_blob.is_none()) {
        return Ok(false);
    }

    let Some(req) =
        build_connector_hdr_req(device, connector, support, hdr_enabled, hdr_metadata_blob)?
    else {
        return Ok(true);
    };

    let flags =
        drm::control::AtomicCommitFlags::TEST_ONLY | drm::control::AtomicCommitFlags::ALLOW_MODESET;

    Ok(device.atomic_commit(flags, req).is_ok())
}

fn apply_connector_hdr_state(
    device: &impl drm::control::Device,
    connector: connector::Handle,
    support: &HdrSupport,
    hdr_enabled: bool,
    hdr_metadata_blob: Option<u64>,
) -> Result<ConnectorHdrApplyResult, anyhow::Error> {
    if hdr_enabled && (!support.can_enable() || hdr_metadata_blob.is_none()) {
        return Ok(ConnectorHdrApplyResult::Rejected);
    }

    let Some(req) =
        build_connector_hdr_req(device, connector, support, hdr_enabled, hdr_metadata_blob)?
    else {
        return Ok(ConnectorHdrApplyResult::Unchanged);
    };

    if !test_connector_hdr_state(device, connector, support, hdr_enabled, hdr_metadata_blob)? {
        return Ok(ConnectorHdrApplyResult::Rejected);
    }

    device
        .atomic_commit(drm::control::AtomicCommitFlags::ALLOW_MODESET, req)
        .map_err(|err| anyhow!("failed to apply HDR connector state: {err}"))?;

    Ok(ConnectorHdrApplyResult::Applied)
}

fn parse_edid_identity(edid: &[u8]) -> Option<EdidMonitorIdentity> {
    if edid.len() < 128 || edid.get(0..8)? != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return None;
    }

    let manufacturer = u16::from_be_bytes([edid[8], edid[9]]);
    let make = [
        (((manufacturer >> 10) & 0x1f) as u8 + b'A' - 1) as char,
        (((manufacturer >> 5) & 0x1f) as u8 + b'A' - 1) as char,
        ((manufacturer & 0x1f) as u8 + b'A' - 1) as char,
    ]
    .iter()
    .collect::<String>();

    let product_code = u16::from_le_bytes([edid[10], edid[11]]);
    let numeric_serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    let mut monitor_name = None;
    let mut descriptor_serial = None;

    for descriptor in edid[54..126].chunks_exact(18) {
        if descriptor[0..3] != [0, 0, 0] {
            continue;
        }

        match descriptor[3] {
            0xfc => monitor_name = edid_descriptor_text(descriptor),
            0xff => descriptor_serial = edid_descriptor_text(descriptor),
            _ => {}
        }
    }

    let model = monitor_name.unwrap_or_else(|| format!("0x{product_code:04x}"));
    let serial_number = descriptor_serial.unwrap_or_else(|| {
        if numeric_serial != 0 {
            numeric_serial.to_string()
        } else {
            "unknown".to_string()
        }
    });

    Some(EdidMonitorIdentity {
        make,
        model,
        serial_number,
    })
}

fn edid_descriptor_text(descriptor: &[u8]) -> Option<String> {
    let text = descriptor.get(5..18)?;
    let end = text
        .iter()
        .position(|byte| matches!(*byte, b'\n' | b'\r' | 0))
        .unwrap_or(text.len());
    let value = String::from_utf8_lossy(&text[..end]).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn dispatch_backend_input_event<B: smithay::backend::input::InputBackend>(
    state: &mut DesktopState,
    input: &smithay::backend::input::InputEvent<B>,
) {
    use smithay::backend::input::InputEvent;

    let output_id = state
        .output_at_logical_point(state.pointer_pos)
        .unwrap_or(state.focused_output);
    let scale = state
        .outputs
        .get(&output_id)
        .map(|o| o.scale_factor)
        .unwrap_or(1.0);

    // Absolute devices (touchpad/tablet in absolute mode) must transform against the
    // focused output's geometry, not the combined desktop bounds (anvil/smallvil pattern).
    let clamp_rect = match input {
        InputEvent::PointerMotionAbsolute { .. } => {
            state.pointer_transform_rect_for_output(output_id)
        }
        _ => state.logical_pointer_clamp_rect(),
    };

    if let Some(mut event) = translate_backend_input(
        input,
        state.input.pointer_pos,
        clamp_rect,
        scale,
        state.input.modifiers,
    ) {
        if matches!(input, InputEvent::PointerMotion { .. }) {
            if let crate::core::input::FlowInputEvent::PointerMoved { position } = &mut event {
                let scale = scale.max(1.0);
                let old_pos = state.input.pointer_pos;
                let scaled = Point::<f64, Logical>::from((
                    old_pos.x + (position.x - old_pos.x) / scale,
                    old_pos.y + (position.y - old_pos.y) / scale,
                ));
                let min_x = clamp_rect.loc.x as f64;
                let min_y = clamp_rect.loc.y as f64;
                let max_x = (clamp_rect.loc.x + clamp_rect.size.w) as f64 - f64::EPSILON;
                let max_y = (clamp_rect.loc.y + clamp_rect.size.h) as f64 - f64::EPSILON;
                *position = Point::from((
                    scaled.x.clamp(min_x, max_x.max(min_x)),
                    scaled.y.clamp(min_y, max_y.max(min_y)),
                ));
            }
        }
        state.handle_input(event);
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    flog("FOCALDESK: entered DRM backend");
    let mut event_loop: EventLoop<DrmLoopData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    //
    // Session / seat ownership
    //
    let (session, _notifier) =
        LibSeatSession::new().map_err(|e| anyhow!("Could not initialize libseat session: {e}"))?;

    //
    // Shared compositor state
    //
    let desktop = bootstrap_compositor_core(None, BackendKind::Drm)?;

    //
    // Pick primary KMS device (same node udev will report for the card — open only in `device_added`).
    //
    let primary_node = primary_drm_node(&session)?;
    flog(&format!(
        "Primary DRM node for seat {}: {:?}",
        session.seat(),
        primary_node
    ));

    //
    // libinput
    //
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|e| anyhow!("Failed to assign libinput seat: {:?}", e))?;

    let libinput_backend = LibinputInputBackend::new(libinput.clone());

    //
    // udev
    //
    let udev = UdevBackend::new(session.seat())
        .map_err(|e| anyhow!("Failed to initialize udev backend: {e}"))?;

    #[cfg(feature = "xwayland")]
    let xwayland_event_loop = EventLoop::<DesktopState>::try_new()?;

    let mut data = DrmLoopData {
        core: CompositorCore {
            display: desktop.display,
            #[cfg(feature = "xwayland")]
            xwayland_event_loop,
            listener: desktop.listener,
            wayland_display: desktop.wayland_display,
            state: desktop.state,
            clients: desktop.clients,
            ui_state: desktop.ui_state,
            scene: desktop.scene,
            output_state: desktop.output_state,
            resources: desktop.resources,
            start: desktop.start,
            last_now: desktop.last_now,
        },
        backend: DrmBackend {
            session: session.clone(),
            devices: HashMap::new(),
        },
        libinput,
        should_stop: false,
    };

    let _libinput_token = loop_handle.insert_source(libinput_backend, |event, _, data| {
        if let InputEvent::Keyboard { event, .. } = &event {
            let keycode = event.key_code();
            let state = event.state();
            if state == KeyState::Pressed && (keycode == 1u32.into() || keycode == 9u32.into()) {
                flog("Emergency exit: ESC pressed");
                data.should_stop = true;
                data.core.state.running = false;
                return;
            }
            flog(&format!("key event: code={:?} state={:?}", keycode, state));
        }

        dispatch_backend_input_event::<LibinputInputBackend>(&mut data.core.state, &event);
    })?;

    for (device_id, path) in udev.device_list() {
        let node = DrmNode::from_dev_id(device_id)
            .map_err(|e| anyhow!("Failed to build DrmNode from dev id {device_id:?}: {e}"))?;

        if node != primary_node {
            flog(&format!("Skipping non-primary DRM node {}", path.display()));
            continue;
        }

        device_added(&mut data, &loop_handle, node, &path)?;
    }

    #[cfg(feature = "xwayland")]
    {
        start_xwayland(
            &mut data.core.state,
            &data.core.display.handle(),
            data.core.xwayland_event_loop.handle(),
        )?;
        finish_xwayland_startup(
            &mut data.core.xwayland_event_loop,
            &mut data.core.display,
            &mut data.core.state,
            Duration::from_secs(30),
        )?;
        if let Some(display) = data.core.state.xwayland_display.as_deref() {
            flog(&format!(
                "DRM backend: XWayland active on DISPLAY={display}"
            ));
        } else {
            flog("DRM backend: XWayland is not active (startup failed or disabled)");
        }
    }

    //let (renderer, mut framebuffer) = acquire_drm_framebuffer(...)?
    //let prepared = prepare_active_output(...)?;
    //let mut frame = renderer.render(&mut framebuffer, buffer_size, Transform::Normal /* or whatever is correct */)?;
    //draw_active_output(...)?;
    //frame.finish()?;
    //present_drm_framebuffer(...)?

    //
    // Main loop
    //
    while !data.should_stop && data.core.state.running {
        #[cfg(feature = "xwayland")]
        data.core
            .xwayland_event_loop
            .dispatch(Some(Duration::ZERO), &mut data.core.state)?;

        data.core.state.process_settings_ipc_requests();
        data.core.state.process_chrome_timers();
        data.core.state.process_notification_timers();
        data.core.state.process_lock_timers();

        event_loop.dispatch(Some(Duration::from_millis(16)), &mut data)?;

        if let Some(stream) = data.core.listener.accept()? {
            let client = data.core.display.handle().insert_client(
                stream,
                std::sync::Arc::new(crate::core::wayland::client::ClientState::default()),
            )?;
            data.core.clients.push(client);
        }

        let now = Instant::now();
        let dt = now.saturating_duration_since(data.core.last_now);
        {
            let core = &mut data.core;
            let backend = &mut data.backend;

            if let Some(device) = backend.devices.values_mut().next() {
                core.state.begin_portal_dispatch(
                    &mut device.renderer,
                    &mut core.ui_state,
                    &mut core.scene,
                    &core.output_state,
                    now,
                    dt,
                );
            }
        }
        if data.core.state.wayland_clients_may_dispatch() {
            data.core.display.dispatch_clients(&mut data.core.state)?;
        }
        data.core.state.end_portal_dispatch();

        data.core.state.refresh_space();
        data.core.display.handle().flush_clients()?;
        data.core.state.tick_layout();

        let screenshot_output = data.core.state.screenshot_request();
        data.core
            .state
            .image_copy_capture_sessions
            .retain(|session| session.alive());
        let portal_active = !data.core.state.image_copy_capture_sessions.is_empty()
            && !data.core.state.pending_portal_captures.is_empty();
        let should_render = data.core.state.needs_redraw()
            || screenshot_output.is_some()
            || data.core.state.screenshot_all_requested
            || portal_active;
        if !should_render {
            if portal_active {
                if let Some(device) = data.backend.devices.values_mut().next() {
                    crate::core::portal::complete_pending_portal_captures(
                        &mut data.core.state,
                        &mut device.renderer,
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                        now,
                        dt,
                    );
                }
            }
            continue;
        }
        data.core.last_now = now;

        for (_node, device) in data.backend.devices.iter_mut() {
            for (_crtc, surface) in device.surfaces.iter_mut() {
                let owns_cursor = data.core.state.output_owns_cursor(surface.output_id);
                let pending_damage = data.core.state.output_has_pending_damage(surface.output_id);
                let wants_screenshot = screenshot_output == Some(surface.output_id);
                let should_skip = !data.core.state.render.redraw_all
                    && !data.core.state.screenshot_all_requested
                    && !portal_active
                    && !pending_damage
                    && !wants_screenshot
                    && !owns_cursor;

                if should_skip {
                    continue;
                }

                data.core.state.drm_try_pass_cursor_this_frame = owns_cursor
                    && data.core.state.drm_submit_hw_cursor
                    && data.core.state.cursor_manager.visible();

                let buffer_size = Size::from((surface.size.w, surface.size.h));

                if portal_active {
                    data.core.state.render.redraw_all = true;
                }
                if let Some(out) = data.core.state.outputs.get_mut(&surface.output_id) {
                    out.hdr_supported =
                        surface.hdr_support.can_enable() && surface.hdr_render_supported;
                    let mut hdr_target = out.hdr_requested && out.hdr_supported;
                    if hdr_target {
                        if let Some(format) = surface.hdr_offscreen_format {
                            if let Err(err) = ensure_offscreen_texture(
                                &mut device.renderer,
                                &mut surface.offscreen,
                                surface.size,
                                format,
                            ) {
                                flog(&format!(
                                    "HDR offscreen texture failed for {:?} format {:?}: {err}; keeping SDR",
                                    surface.output_id, format
                                ));
                                hdr_target = false;
                                out.hdr_supported = false;
                                out.hdr_enabled = false;
                            }
                        } else {
                            hdr_target = false;
                            out.hdr_supported = false;
                            out.hdr_enabled = false;
                        }
                    }
                    if hdr_target != surface.hdr_enabled_applied {
                        let device = device.drm_output_manager.device();
                        let hdr_metadata_blob = if hdr_target {
                            match ensure_hdr_metadata_blob(
                                device,
                                &surface.hdr_support,
                                &mut surface.hdr_metadata_blob,
                            ) {
                                Ok(blob) => Some(blob),
                                Err(err) => {
                                    flog(&format!(
                                        "Failed to create HDR metadata blob for {:?}: {err}",
                                        surface.output_id
                                    ));
                                    out.hdr_supported = false;
                                    out.hdr_enabled = false;
                                    surface.hdr_enabled_applied = false;
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        match apply_connector_hdr_state(
                            device,
                            surface.connector,
                            &surface.hdr_support,
                            hdr_target,
                            hdr_metadata_blob,
                        ) {
                            Ok(ConnectorHdrApplyResult::Applied) => {
                                out.hdr_enabled = hdr_target;
                                surface.hdr_enabled_applied = hdr_target;
                                if !hdr_target {
                                    destroy_hdr_metadata_blob(
                                        device,
                                        surface.hdr_metadata_blob.take(),
                                    );
                                }
                            }
                            Ok(ConnectorHdrApplyResult::Unchanged) => {
                                out.hdr_enabled = hdr_target;
                                surface.hdr_enabled_applied = hdr_target;
                                if !hdr_target {
                                    destroy_hdr_metadata_blob(
                                        device,
                                        surface.hdr_metadata_blob.take(),
                                    );
                                }
                            }
                            Ok(ConnectorHdrApplyResult::Rejected) => {
                                if hdr_target {
                                    flog(&format!(
                                        "HDR connector state rejected by atomic test for {:?}; keeping SDR",
                                        surface.output_id
                                    ));
                                    out.hdr_supported = false;
                                    out.hdr_enabled = false;
                                    surface.hdr_enabled_applied = false;
                                    destroy_hdr_metadata_blob(
                                        device,
                                        surface.hdr_metadata_blob.take(),
                                    );
                                } else {
                                    flog(&format!(
                                        "SDR connector state rejected by atomic test for {:?}",
                                        surface.output_id
                                    ));
                                }
                            }
                            Err(err) => {
                                flog(&format!(
                                    "Failed to update HDR connector state for {:?}: {err}",
                                    surface.output_id
                                ));
                            }
                        }
                    } else {
                        out.hdr_enabled = surface.hdr_enabled_applied;
                    }
                }
                let prepared = prepare_output(
                    &mut data.core.state,
                    &mut device.renderer,
                    surface.output_id,
                    buffer_size,
                    &mut data.core.ui_state,
                    now,
                    dt,
                    portal_active,
                )?;

                ensure_offscreen_texture(
                    &mut device.renderer,
                    &mut surface.offscreen,
                    surface.size,
                    if surface.hdr_enabled_applied {
                        surface.hdr_offscreen_format.unwrap_or(SDR_OFFSCREEN_FORMAT)
                    } else {
                        SDR_OFFSCREEN_FORMAT
                    },
                )?;
                if surface.hdr_enabled_applied {
                    ensure_offscreen_texture(
                        &mut device.renderer,
                        &mut surface.sdr_offscreen,
                        surface.size,
                        SDR_OFFSCREEN_FORMAT,
                    )?;
                } else {
                    surface.sdr_offscreen = None;
                }

                {
                    let render_offscreen = if surface.hdr_enabled_applied {
                        surface.sdr_offscreen.as_mut()
                    } else {
                        surface.offscreen.as_mut()
                    }
                    .ok_or_else(|| anyhow!("offscreen texture missing before draw"))?;

                    let mut target = device
                        .renderer
                        .bind(&mut render_offscreen.texture)
                        .map_err(|e| anyhow!("bind offscreen for draw: {e}"))?;

                    let client_elements = build_output_client_elements(
                        &mut data.core.state,
                        &mut device.renderer,
                        surface.output_id,
                    );
                    let popup_elements = build_output_popup_elements(
                        &mut data.core.state,
                        &mut device.renderer,
                        surface.output_id,
                    );

                    let mut frame = device
                        .renderer
                        .render(&mut target, buffer_size, Transform::Normal)
                        .map_err(|e| anyhow!("begin offscreen frame: {e}"))?;

                    draw_output(
                        &mut data.core.state,
                        &mut frame,
                        &prepared,
                        &client_elements,
                        &popup_elements,
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                    )?;

                    let sync = frame.finish()?;
                    if portal_active {
                        device.renderer.wait(&sync)?;
                    }
                }

                if surface.hdr_enabled_applied {
                    let shader = data
                        .core
                        .state
                        .render
                        .chrome_shaders
                        .sdr_to_hdr_pq
                        .as_ref()
                        .ok_or_else(|| anyhow!("SDR-to-HDR shader missing"))?;
                    let sdr_texture = surface
                        .sdr_offscreen
                        .as_ref()
                        .ok_or_else(|| anyhow!("SDR offscreen missing before HDR conversion"))?
                        .texture
                        .clone();
                    let hdr_texture = &mut surface
                        .offscreen
                        .as_mut()
                        .ok_or_else(|| anyhow!("HDR offscreen missing before HDR conversion"))?
                        .texture;
                    convert_sdr_scene_to_hdr(
                        &mut device.renderer,
                        &sdr_texture,
                        hdr_texture,
                        surface.size,
                        shader,
                    )?;
                }

                if let Some(texture) = capture_source_texture(surface).cloned() {
                    data.core.state.portal_capture_source.insert(
                        surface.output_id,
                        crate::core::portal::PortalCaptureSource {
                            texture,
                            size: surface.size,
                            captured_at: now,
                        },
                    );
                }

                if portal_active {
                    crate::core::portal::complete_pending_portal_captures_for_output(
                        &mut data.core.state,
                        &mut device.renderer,
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                        surface.output_id,
                        now,
                        dt,
                    );
                }

                if wants_screenshot {
                    data.core.state.screenshot_seq += 1;
                    let seq = data.core.state.screenshot_seq;
                    let output_name = format!("output-{}", surface.output_id.0);

                    match save_offscreen_screenshot(
                        &mut device.renderer,
                        surface,
                        &output_name,
                        seq,
                    ) {
                        Ok(path) => {
                            flog(&format!("Screenshot saved to {}", path.display()));
                        }
                        Err(err) => {
                            flog(&format!("Screenshot failed: {err}"));
                        }
                    }
                    data.core.state.clear_screenshot_request(surface.output_id);
                }

                // now borrow immutably after the mutable borrow is gone
                let texture = present_source_texture(surface)
                    .expect("offscreen texture missing")
                    .clone();
                let output_scale = data
                    .core
                    .state
                    .outputs
                    .get(&surface.output_id)
                    .map(|o| o.scale_factor)
                    .unwrap_or(1.0)
                    .max(1.0);
                let present_logical_size = Size::<i32, Logical>::from((
                    (surface.size.w as f64 / output_scale).round() as i32,
                    (surface.size.h as f64 / output_scale).round() as i32,
                ));
                let present_src = Rectangle::<f64, Logical>::from_loc_and_size(
                    (0.0, 0.0),
                    (surface.size.w as f64, surface.size.h as f64),
                );
                surface
                    .present_damage
                    .add(prepared.frame_ctx.damage.iter().map(|rect| {
                        Rectangle::<i32, Buffer>::from_loc_and_size(
                            (rect.loc.x, rect.loc.y),
                            (rect.size.w, rect.size.h),
                        )
                    }));
                let present_damage = surface.present_damage.snapshot();

                let texture_elem = TextureRenderElement::from_texture_with_damage(
                    surface.present_render_id.clone(),
                    device.renderer.context_id(),
                    (0.0, 0.0),
                    texture,
                    1,
                    Transform::Normal,
                    Some(1.0),
                    Some(present_src),
                    Some(present_logical_size),
                    None,
                    present_damage,
                    Kind::Unspecified,
                );

                let mut present_elements: Vec<DrmPresentElement> =
                    Vec::with_capacity(if data.core.state.drm_try_pass_cursor_this_frame {
                        2
                    } else {
                        1
                    });

                if data.core.state.drm_try_pass_cursor_this_frame {
                    if let Some(ct) = data.core.state.render.sw_cursor_texture.as_ref() {
                        let scale = data
                            .core
                            .state
                            .outputs
                            .get(&surface.output_id)
                            .map(|o| o.scale)
                            .unwrap_or_else(|| Scale::from((1.0, 1.0)));
                        let rel = data
                            .core
                            .state
                            .pointer_relative_to_output_logical(surface.output_id)
                            .unwrap_or(data.core.state.pointer_pos);
                        let phys: Point<i32, Physical> =
                            rel.to_physical_precise_round::<f64, i32>(scale);
                        let (hx, hy) = data.core.state.render.sw_cursor_hotspot;
                        let (tw, th) = data.core.state.render.sw_cursor_tex_size;
                        let cursor_logical_size = Size::<i32, Logical>::from((
                            (tw as f64 / output_scale).round().max(1.0) as i32,
                            (th as f64 / output_scale).round().max(1.0) as i32,
                        ));
                        let cursor_src = Rectangle::<f64, Logical>::from_loc_and_size(
                            (0.0, 0.0),
                            (tw as f64, th as f64),
                        );
                        let cursor_elem = TextureRenderElement::from_static_texture(
                            data.core.state.drm_cursor_render_id.clone(),
                            device.renderer.context_id(),
                            Point::<f64, Physical>::from((
                                (phys.x - hx) as f64,
                                (phys.y - hy) as f64,
                            )),
                            ct.clone(),
                            1,
                            Transform::Normal,
                            Some(1.0),
                            Some(cursor_src),
                            Some(cursor_logical_size),
                            None,
                            Kind::Cursor,
                        );
                        present_elements.push(DrmPresentElement::Texture(cursor_elem));
                    }
                }

                present_elements.push(DrmPresentElement::Texture(texture_elem));

                let frame_result = match surface.drm_output.render_frame(
                    &mut device.renderer,
                    &present_elements,
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                    FrameFlags::DEFAULT,
                ) {
                    Ok(frame_result) => frame_result,
                    Err(err) if surface.hdr_enabled_applied => {
                        flog(&format!(
                            "HDR render_frame failed for {:?}: {err}; restoring SDR",
                            surface.output_id
                        ));
                        let drm_device = device.drm_output_manager.device();
                        if let Err(restore_err) = apply_connector_hdr_state(
                            drm_device,
                            surface.connector,
                            &surface.hdr_support,
                            false,
                            None,
                        ) {
                            flog(&format!(
                                "Failed to restore SDR connector state for {:?}: {restore_err}",
                                surface.output_id
                            ));
                        }
                        destroy_hdr_metadata_blob(drm_device, surface.hdr_metadata_blob.take());
                        surface.hdr_enabled_applied = false;
                        surface.hdr_render_supported = false;
                        if let Some(out) = data.core.state.outputs.get_mut(&surface.output_id) {
                            out.hdr_supported = false;
                            out.hdr_enabled = false;
                        }
                        surface.offscreen = None;
                        surface.sdr_offscreen = None;
                        data.core
                            .state
                            .mark_output_full_damage(surface.output_id, DamageSource::Unknown);
                        continue;
                    }
                    Err(err) => return Err(err.into()),
                };

                data.core.state.update_cursor_policy_after_drm_present(
                    &frame_result.states,
                    frame_result.cursor_element.is_some(),
                );

                if !frame_result.is_empty {
                    surface.drm_output.queue_frame(None)?;
                    data.core.state.compositor_ready = true;
                }
            }

            // screen capture all outputs
            if data.core.state.screenshot_all_requested {
                data.core.state.screenshot_seq += 1;
                let seq = data.core.state.screenshot_seq;

                match save_all_outputs_screenshot(&mut device.renderer, &mut device.surfaces, seq) {
                    Ok(path) => flog(&format!(
                        "All-outputs screenshot saved to {}",
                        path.display()
                    )),
                    Err(err) => flog(&format!("All-outputs screenshot failed: {err}")),
                }
            }
        }

        if portal_active {
            if let Some(device) = data.backend.devices.values_mut().next() {
                crate::core::portal::complete_pending_portal_captures(
                    &mut data.core.state,
                    &mut device.renderer,
                    &mut data.core.ui_state,
                    &data.core.scene,
                    &data.core.output_state,
                    now,
                    dt,
                );
            }
        }

        data.core.state.screenshot_all_requested = false;

        data.core.state.clear_repaint_request();
        data.core.state.render.frame_no += 1;

        let frame_time_ms = data.core.start.elapsed().as_millis() as u32;
        data.core.state.send_frame_callbacks(frame_time_ms);
    }

    Ok(())
}

fn connector_name(info: &drm::control::connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

fn dmabuf_capture_formats(format_set: &FormatSet) -> Vec<(Fourcc, Vec<Modifier>)> {
    let mut formats: Vec<(Fourcc, Vec<Modifier>)> = Vec::new();
    for format in format_set.iter() {
        if let Some((_, modifiers)) = formats.iter_mut().find(|(code, _)| *code == format.code) {
            if !modifiers.contains(&format.modifier) {
                modifiers.push(format.modifier);
            }
        } else {
            formats.push((format.code, vec![format.modifier]));
        }
    }
    formats
}

pub fn make_drm_gpu(
    node: DrmNode,
    fd: DrmDeviceFd,
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    registration_token: calloop::RegistrationToken,
) -> Result<DrmGpu> {
    let render_node = {
        let display = unsafe { EGLDisplay::new(gbm.clone())? };
        let egl_device = EGLDevice::device_for_display(&display)?;
        let _context = EGLContext::new(&display)?;
        if egl_device.is_software() {
            None
        } else {
            egl_device
                .try_get_render_node()
                .ok()
                .flatten()
                .or(Some(node))
        }
    };

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );

    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), render_node.into());

    let color_formats = [
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
        Fourcc::Argb2101010,
        Fourcc::Xrgb2101010,
    ];

    let render_formats = FormatSet::default();

    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator.clone(),
        framebuffer_exporter.clone(),
        Some(gbm.clone()),
        color_formats.into_iter(),
        render_formats,
    );

    Ok(DrmGpu {
        node,
        fd,
        gbm,
        registration_token,
        render_node,
        allocator,
        framebuffer_exporter,
        drm_output_manager,
        outputs: HashMap::new(),
    })
}

fn device_added(
    data: &mut DrmLoopData,
    loop_handle: &LoopHandle<'_, DrmLoopData>,
    node: DrmNode,
    path: &Path,
) -> Result<()> {
    flog(&format!(
        "DRM device added: {:?} ({})",
        node,
        path.display()
    ));
    flog(&format!(
        "About to open device-added DRM node {}",
        path.display()
    ));
    // Open device through the active seat
    let fd = match data
        .backend
        .session
        .open(path, OFlags::RDWR | OFlags::CLOEXEC)
    {
        Ok(fd) => fd,
        Err(err) => {
            flog(&format!(
                "Failed to open DRM device {} via libseat/session: {:?}",
                path.display(),
                err
            ));
            return Err(anyhow!(
                "Failed to open DRM device {}: {:?}",
                path.display(),
                err
            ));
        }
    };

    flog("Device-added DRM node open succeeded");

    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, notifier) =
        DrmDevice::new(fd.clone(), true).context("Failed to create DrmDevice for added node")?;
    let gbm = GbmDevice::new(fd.clone()).context("Failed to create GBM device for node")?;

    let egl_display = unsafe { egl::EGLDisplay::new(gbm.clone()) }
        .context("Failed to create EGLDisplay for DRM node")?;
    let egl_device =
        EGLDevice::device_for_display(&egl_display).context("Failed to query EGLDevice")?;

    if egl_device.is_software() {
        flog(
            "EGL reports a software rasterizer (e.g. llvmpipe). Check drivers if you expected GPU acceleration.",
        );
    }

    // Prefer the render node from the EGL driver so scan-out import matches where GL allocates.
    let render_node_for_gpu = if egl_device.is_software() {
        None
    } else {
        egl_device
            .try_get_render_node()
            .ok()
            .flatten()
            .or(Some(node))
    };

    let egl_context = EGLContext::new_with_priority(&egl_display, ContextPriority::High)
        .context("Failed to create EGLContext for DRM node")?;
    let mut renderer = unsafe { GlesRenderer::new(egl_context) }
        .context("Failed to create GLES renderer for DRM node")?;

    match renderer.bind_wl_display(&data.core.display.handle()) {
        Ok(_) => flog("EGL Wayland display bound for DRM renderer"),
        Err(err) => flog(&format!(
            "Failed to bind EGL Wayland display for DRM renderer: {err:?}"
        )),
    }

    if data.core.state.dmabuf_global.is_none() {
        let dmabuf_node = render_node_for_gpu.unwrap_or(node);
        let default_feedback =
            DmabufFeedbackBuilder::new(dmabuf_node.dev_id(), renderer.dmabuf_formats())
                .build()
                .context("Failed to build linux-dmabuf feedback")?;
        let global = data
            .core
            .state
            .dmabuf_state
            .create_global_with_default_feedback::<DesktopState>(
                &data.core.display.handle(),
                &default_feedback,
            );

        data.core.state.dmabuf_global = Some(global);
        data.core.state.dmabuf_node = Some(dmabuf_node);
        flog(&format!(
            "linux-dmabuf enabled on DRM node {:?}",
            dmabuf_node
        ));
    }

    let render_format_set = renderer.egl_context().dmabuf_render_formats().clone();
    data.core.state.portal_dmabuf_formats = dmabuf_capture_formats(&render_format_set);
    let render_formats = render_format_set.iter().copied().collect::<Vec<_>>();

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), render_node_for_gpu.into());
    let mut drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        framebuffer_exporter,
        Some(gbm.clone()),
        [
            Fourcc::Argb8888,
            Fourcc::Xrgb8888,
            Fourcc::Argb2101010,
            Fourcc::Xrgb2101010,
        ],
        render_formats,
    );

    let mut surfaces = HashMap::new();

    // Scan connectors and attach anything connected
    let res = drm_output_manager
        .device()
        .resource_handles()
        .context("Failed to get DRM resources")?;

    let mut used_crtcs = std::collections::HashSet::new();
    let mut initialized_one = false;

    let mut id: u64 = 1;

    let mut next_x = 0;
    let configured_displays = load_display_config();

    for conn in res.connectors() {
        let info = drm_output_manager
            .device()
            .get_connector(*conn, false)
            .context("Failed to get connector info")?;

        if info.state() != drm::control::connector::State::Connected {
            continue;
        }

        //flog(&format!("Found connected connector: {:?}", conn));
        let name = connector_name(&info);

        let connector_size = info.size();
        flog(&format!(
            "CONNECTOR: name={} handle={:?} state={:?} size_mm={}x{} encoders={:?} modes={}",
            name,
            info.handle(),
            info.state(),
            connector_size.map(|size| size.0).unwrap_or(0),
            connector_size.map(|size| size.1).unwrap_or(0),
            info.encoders(),
            info.modes().len(),
        ));

        for (i, m) in info.modes().iter().enumerate() {
            flog(&format!(
                "  mode[{}]: {}x{} @ {}Hz type={:?}",
                i,
                m.size().0,
                m.size().1,
                m.vrefresh(),
                m.mode_type(),
            ));
        }

        let mode = info
            .modes()
            .iter()
            .find(|m| {
                m.mode_type()
                    .contains(drm::control::ModeTypeFlags::PREFERRED)
            })
            .cloned()
            .or_else(|| info.modes().first().cloned());

        if let Some(mode) = mode {
            let (w, h) = mode.size();

            flog(&format!("Selected mode: {}x{} @ {}", w, h, mode.vrefresh()));

            let output_name = format!("{}-{}", info.interface().as_str(), info.interface_id());

            let fallback_mm =
                physical_size_mm_from_pixels(Size::<i32, Physical>::from((w as i32, h as i32)));
            let (mm_w, mm_h) = info
                .size()
                .filter(|(mm_w, mm_h)| *mm_w > 0 && *mm_h > 0)
                .map(|(mm_w, mm_h)| (mm_w as i32, mm_h as i32))
                .unwrap_or(fallback_mm);

            let edid = connector_edid(drm_output_manager.device(), *conn);
            let hdr_support =
                connector_hdr_support(drm_output_manager.device(), *conn, edid.as_deref());
            log_hdr_support(&output_name, &hdr_support);

            let edid_identity = edid.as_deref().and_then(parse_edid_identity);
            let make = edid_identity
                .as_ref()
                .map(|identity| identity.make.clone())
                .unwrap_or_else(|| "FocalDesk".to_string());
            let model = edid_identity
                .as_ref()
                .map(|identity| identity.model.clone())
                .unwrap_or_else(|| info.interface().as_str().to_string());
            let serial_number = edid_identity
                .as_ref()
                .map(|identity| identity.serial_number.clone())
                .unwrap_or_else(|| {
                    format!("{}-{}", info.interface().as_str(), info.interface_id())
                });

            let output = Output::new(
                output_name.clone(),
                PhysicalProperties {
                    size: (mm_w, mm_h).into(),
                    subpixel: Subpixel::Unknown,
                    make: make.clone(),
                    model: model.clone(),
                    serial_number: serial_number.clone(),
                },
            );

            // Logical layout in global compositor space (must match `register_output_entry` / `map_output`).
            // wl_output + xdg_output advertise this to clients; leaving (0,0) stacks every head at the origin
            // (e.g. OBS projector shows all DRM outputs on top of each other).
            let origin = Point::<i32, Logical>::from((next_x, 0));
            let output_scale = configured_display_scale(&configured_displays, &output_name);
            let hdr_enabled = configured_display_hdr_enabled(&configured_displays, &output_name);
            let output_scale_int = output_scale.round().max(1.0) as i32;
            let logical_size = Size::<i32, Logical>::from((
                (w as f64 / output_scale).round() as i32,
                (h as f64 / output_scale).round() as i32,
            ));
            let chrome_layout = build_chrome_layout(
                logical_size,
                data.core.state.chrome.metrics.topbar_h,
                data.core.state.chrome.metrics.sidebar_w,
            );

            let refresh_mhz = ((mode.vrefresh() as i32).max(60)) * 1000;
            let wl_mode = WlMode {
                size: (w as i32, h as i32).into(),
                refresh: refresh_mhz,
            };

            output.change_current_state(
                Some(wl_mode),
                Some(Transform::Normal),
                Some(smithay::output::Scale::Custom {
                    advertised_integer: output_scale_int,
                    fractional: output_scale,
                }),
                Some(origin),
            );
            output.set_preferred(wl_mode);
            output.create_global::<DesktopState>(&data.core.display.handle());

            flog(&format!(
                "Wayland output advertised: name={} px={}x{} logical={}x{} scale={} mm={}x{} refresh_mhz={} make={:?} model={:?} serial={:?}",
                output_name,
                w,
                h,
                logical_size.w,
                logical_size.h,
                output_scale,
                mm_w,
                mm_h,
                wl_mode.refresh,
                make,
                model,
                serial_number,
            ));
            flog(&format!(
                "DRM chrome layout: name={} topbar={:?} status_wells={:?} clock={:?}",
                output_name,
                chrome_layout.topbar.outer,
                chrome_layout.topbar.status_wells,
                chrome_layout.topbar.clock_well,
            ));

            let crtc = info.encoders().iter().find_map(|enc| {
                let enc_info = drm_output_manager.device().get_encoder(*enc).ok()?;

                res.filter_crtcs(enc_info.possible_crtcs())
                    .into_iter()
                    .find(|candidate| !used_crtcs.contains(candidate))
            });

            let Some(crtc) = crtc else {
                flog(&format!("No CRTC available for connector {:?}", conn));
                continue;
            };

            used_crtcs.insert(crtc);

            let planes = drm_output_manager
                .device()
                .planes(&crtc)
                .context("Failed to query planes for connector")?;

            let mut allocator = GbmAllocator::new(
                gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );

            let tex_phys_size: Size<i32, Physical> = Size::from((i32::from(w), i32::from(h)));

            let _gbm_buffer = allocator.create_buffer(
                tex_phys_size.w as u32,
                tex_phys_size.h as u32,
                Fourcc::Argb8888,
                &[DrmModifier::Linear],
            )?;
            let hdr_scanout_format = probe_hdr_scanout_format(&mut allocator, tex_phys_size);
            let hdr_offscreen_format = select_hdr_offscreen_format(&mut renderer, tex_phys_size);
            let hdr_render_supported =
                hdr_scanout_format.is_some() && hdr_offscreen_format.is_some();
            if hdr_support.can_enable() {
                flog(&format!(
                    "HDR render probe: output={} scanout={:?} offscreen={:?} supported={}",
                    output_name, hdr_scanout_format, hdr_offscreen_format, hdr_render_supported
                ));
            }

            let drm_output = drm_output_manager
                .lock()
                .initialize_output::<_, SolidColorRenderElement>(
                    crtc,
                    mode,
                    &[*conn],
                    &output,
                    Some(planes),
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                )?;

            let mut hdr_metadata_blob = None;
            let mut hdr_enabled_applied = false;
            let device = drm_output_manager.device();
            let hdr_requested = hdr_enabled;
            let hdr_enabled = hdr_requested && hdr_render_supported;
            let requested_hdr_metadata_blob = if hdr_enabled {
                match ensure_hdr_metadata_blob(device, &hdr_support, &mut hdr_metadata_blob) {
                    Ok(blob) => Some(blob),
                    Err(err) => {
                        flog(&format!(
                            "Failed to create HDR metadata blob for {}: {err}; keeping SDR",
                            output_name
                        ));
                        None
                    }
                }
            } else {
                None
            };
            match apply_connector_hdr_state(
                device,
                *conn,
                &hdr_support,
                hdr_enabled,
                requested_hdr_metadata_blob,
            ) {
                Ok(ConnectorHdrApplyResult::Applied | ConnectorHdrApplyResult::Unchanged) => {
                    hdr_enabled_applied = hdr_enabled && hdr_support.can_enable();
                    if !hdr_enabled_applied {
                        destroy_hdr_metadata_blob(device, hdr_metadata_blob.take());
                    }
                }
                Ok(ConnectorHdrApplyResult::Rejected) => {
                    if hdr_enabled {
                        flog(&format!(
                            "HDR connector state rejected by atomic test for {}; keeping SDR",
                            output_name
                        ));
                        match apply_connector_hdr_state(device, *conn, &hdr_support, false, None) {
                            Ok(
                                ConnectorHdrApplyResult::Applied
                                | ConnectorHdrApplyResult::Unchanged
                                | ConnectorHdrApplyResult::Rejected,
                            ) => {}
                            Err(err) => {
                                flog(&format!(
                                    "Failed to restore SDR connector state for {}: {err}",
                                    output_name
                                ));
                            }
                        }
                        destroy_hdr_metadata_blob(device, hdr_metadata_blob.take());
                    } else {
                        flog(&format!(
                            "SDR connector state rejected by atomic test for {}",
                            output_name
                        ));
                    }
                }
                Err(err) => {
                    flog(&format!(
                        "Failed to apply HDR connector state for {}: {err}",
                        output_name
                    ));
                    destroy_hdr_metadata_blob(device, hdr_metadata_blob.take());
                }
            }

            //let offscreen = OffscreenOutput {
            //    size: tex_phys_size,
            //    texture: None,
            //};

            let output_id = OutputId(id);
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                out.handle = output.clone();
                out.physical_size = Size::<i32, Physical>::from((w as i32, h as i32));
                out.logical_size = logical_size;
                out.scale_factor = output_scale;
                out.scale = smithay::utils::Scale::from((output_scale, output_scale));
            }

            data.core.state.register_output_entry(
                output_id,
                output.clone(),
                origin,
                Size::<i32, Physical>::from((w as i32, h as i32)),
                output_scale,
            );
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                out.hdr_supported = hdr_support.can_enable() && hdr_render_supported;
                out.hdr_requested = hdr_requested;
                out.hdr_enabled = hdr_enabled_applied;
            }

            if !initialized_one {
                data.core.state.primary_output = output_id;
            }

            surfaces.insert(
                crtc,
                DrmSurfaceState {
                    connector: *conn,
                    output,
                    mode: wl_mode,
                    size: Size::<i32, Physical>::from((w as i32, h as i32)),
                    output_id: output_id,
                    origin,
                    present_render_id: Id::new(),
                    present_damage: DamageBag::default(),
                    hdr_support,
                    hdr_render_supported,
                    hdr_offscreen_format,
                    hdr_metadata_blob,
                    hdr_enabled_applied,
                    drm_output,
                    offscreen: None,
                    sdr_offscreen: None,
                },
            );
            id += 1;
            next_x += logical_size.w;
            flog("Output initialized (Wayland + DRM)");
            data.core.state.drm_submit_hw_cursor = true;
            initialized_one = true;
        }
    }

    let registration_token =
        loop_handle.insert_source(notifier, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => {
                if let Some(device) = state.backend.devices.get_mut(&node) {
                    if let Some(surface) = device.surfaces.get_mut(&crtc) {
                        if let Err(err) = surface.drm_output.frame_submitted() {
                            flog(&format!(
                                "frame_submitted failed on {:?}/{:?}: {}",
                                node, crtc, err
                            ));
                        }
                    }
                }
            }
            DrmEvent::Error(err) => {
                flog(&format!("DRM event error on {:?}: {}", node, err));
            }
        })?;

    //data.backend.devices.insert(
    //    node,
    //    DrmDeviceState {
    //        registration_token,
    //        render_node: render_node_for_gpu,
    //       renderer,
    //         drm_output_manager,
    //         surfaces,
    //     },
    // );

    let temp_device = DrmDeviceState {
        registration_token,
        render_node: render_node_for_gpu,
        renderer,
        drm_output_manager,
        surfaces,
    };

    let displays = collect_display_configs(&temp_device, &data.core);

    if let Err(err) = write_display_config(&displays) {
        flog(&format!("Failed to write display config: {err}"));
    }

    if !displays.is_empty() {
        refresh_portal_services(&data.core.state.client_wayland_display);
    }

    data.backend.devices.insert(node, temp_device);

    Ok(())
}
