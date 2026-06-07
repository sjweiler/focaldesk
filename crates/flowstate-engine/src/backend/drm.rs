// DRM/KMS backend — uses the same [`DesktopState`] path as winit via [`crate::backend::common::NestedDesktop`].
// Full session/udev/scanout should follow the Smithay anvil `udev` backend pattern.

use crate::backend::common::{
    bootstrap_compositor_core, finish_xwayland_startup, physical_size_mm_from_pixels,
    start_xwayland, NestedDesktop,
};
use crate::backend::drm::drm::buffer::DrmModifier;
use drm::control::{connector, crtc, Mode};
use smithay::backend::allocator::gbm::GbmBuffer;
use smithay::backend::allocator::Allocator;
use smithay::backend::input::{InputEvent, KeyState};
use smithay::backend::renderer::gles::GlesError;
use smithay::backend::renderer::Offscreen;
use smithay::reexports::drm::control::Device as _;
// `DrmOutput::render_frame` / `initialize_output` drive an internal [`smithay::backend::drm::compositor::DrmCompositor`].

use smithay::backend::input::KeyboardKeyEvent;
//use smithay::backend::renderer::element::{Id, Kind};
use crate::core::backend_render::{build_output_client_elements, prepare_output};
use smithay::backend::renderer::utils::{CommitCounter, DamageBag};
use smithay::backend::renderer::Frame;
//use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    render_elements, texture::TextureRenderElement, Id, Kind,
};

use crate::core::backend_render::PreparedOutput;
use flowstate_flow::keybinds::BackendKind;

// DRM/KMS backend for FlowState.
//
// This is the real hardware backend counterpart to the winit backend.
// It keeps compositor state shared, but owns its own session/device/input/output plumbing.
//
// This is intentionally a bring-up skeleton:
// - struct layout is real
// - event loop wiring is real
// - device/output attach points are real
// - many internals are still TODO so you can connect them to your existing FlowState code

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
use flowstate_flow::Keybinds;
use flowstate_logging::flog;
use flowstate_notifications::NotificationManager;
use flowstate_resources::RenderResources;
use flowstate_types::OutputId;
use flowstate_ui::chrome::{Chrome, ChromeMetrics};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::{Renderer, Texture as SmithayTexture};

use smithay::{
    backend::{
        allocator::{dmabuf::Dmabuf, gbm::GbmAllocator, gbm::GbmBufferFlags, Fourcc},
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, GbmBufferedSurface},
        egl::{self, context::ContextPriority, EGLContext},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::Error as OutputDamageTrackerError,
            element::memory::MemoryRenderBuffer,
            element::solid::SolidColorRenderElement,
            gles::{Capability, GlesRenderer, GlesTarget, GlesTexture},
            Color32F, ExportMem, ImportDma, ImportEgl, ImportMemWl,
        },
        session::{
            libseat::{self, LibSeatSession},
            Event as SessionEvent, Session,
        },
        udev::{primary_gpu, UdevBackend, UdevEvent},
    },
    desktop::utils::OutputPresentationFeedback,
    input::{keyboard::LedState, SeatState},
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop,
        gbm::Device as GbmDevice,
        input::{event::EventTrait, Libinput},
        wayland_server::{Client, Display, DisplayHandle, ListeningSocket},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::{
        compositor::CompositorState, dmabuf::DmabufFeedbackBuilder, output::OutputManagerState,
        selection::data_device::DataDeviceState, shell::xdg::XdgShellState, shm::ShmState,
    },
};

use smithay::backend::drm::{
    compositor::{FrameError, FrameFlags},
    exporter::gbm::{GbmFramebufferExporter, NodeFilter},
    output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
};

use crate::backend::common::bind_wayland_socket;
use crate::core::{
    desktop::{DesktopInit, DesktopState},
    render::{FlowRenderElement, RenderState},
    ui_state::UiState,
    OutputState, SceneState,
};

use smithay::backend::egl::{EGLDevice, EGLDisplay};

use nix::fcntl::OFlag;
use smithay::reexports::drm;

use smithay::reexports::rustix::fs::OFlags;

use chrono::Local;
use image::{ImageBuffer, Rgba};
use std::fs;

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
    pub output: Output,
    pub mode: WlMode,
    pub size: Size<i32, Physical>,
    pub output_id: OutputId,
    pub origin: Point<i32, Logical>,
    pub present_render_id: Id,
    pub present_damage: DamageBag<i32, Buffer>,

    pub drm_output: DrmOutput<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        Option<OutputPresentationFeedback>,
        DrmDeviceFd,
    >,
    pub offscreen: Option<OffscreenOutput>,
}

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
pub struct DrmLoopData {
    pub core: CompositorCore,
    pub backend: DrmBackend,
    pub libinput: Libinput,
    pub should_stop: bool,
}

fn write_display_config(displays: &[DisplayConfig]) -> Result<()> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });

    let dir = base.join("flowstate");
    std::fs::create_dir_all(&dir)?;

    let path = dir.join("displays.json");

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
        .offscreen
        .as_mut()
        .ok_or_else(|| anyhow!("offscreen texture missing for capture"))?;

    let target = renderer
        .bind(&mut offscreen.texture)
        .map_err(|e| anyhow!("bind offscreen for capture: {e}"))?;

    copy_framebuffer_target_to_png_rgba(renderer, &target, offscreen.size.w, offscreen.size.h)
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
    let filename = format!("flowstate-{}-{}-{}.png", output_name, timestamp, seq);
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
    let filename = format!("flowstate-{}-{}-{}.png", output_name, timestamp, seq);
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
) -> Result<(), GlesError> {
    let recreate = match offscreen {
        Some(existing) => {
            existing.size != size || existing.texture.format() != Some(Fourcc::Abgr8888)
        }
        None => true,
    };

    if recreate {
        let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
        // RGBA8 + RGBA readback is widely supported; BGRA_EXT offscreen reads have regressed to zeros on some GLES.
        let texture = renderer.create_buffer(Fourcc::Abgr8888, tex_size)?;
        *offscreen = Some(OffscreenOutput { size, texture });
    }

    Ok(())
}

pub fn collect_display_configs(
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

    if let Some(event) = translate_backend_input(
        input,
        state.input.pointer_pos,
        clamp_rect,
        scale,
        state.input.modifiers,
    ) {
        state.handle_input(event);
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    flog("FLOWSTATE: entered DRM backend");
    let mut event_loop: EventLoop<DrmLoopData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    //
    // Session / seat ownership
    //
    let (mut session, notifier) =
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

        let screenshot_output = data.core.state.take_screenshot_request();
        let should_render = data.core.state.needs_redraw()
            || screenshot_output.is_some()
            || data.core.state.screenshot_all_requested;
        if !should_render {
            continue;
        }
        data.core.last_now = now;

        for (_node, device) in data.backend.devices.iter_mut() {
            let drm_surface_count = device.surfaces.len();

            for (_crtc, surface) in device.surfaces.iter_mut() {
                let owns_cursor = data.core.state.output_contains_pointer(surface.output_id);
                let pending_damage = data.core.state.output_has_pending_damage(surface.output_id);
                let wants_screenshot = screenshot_output == Some(surface.output_id);
                let should_skip = !data.core.state.render.redraw_all
                    && !data.core.state.screenshot_all_requested
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

                let prepared = prepare_output(
                    &mut data.core.state,
                    &mut device.renderer,
                    surface.output_id,
                    buffer_size,
                    &mut data.core.ui_state,
                    now,
                    dt,
                )?;

                ensure_offscreen_texture(
                    &mut device.renderer,
                    &mut surface.offscreen,
                    surface.size,
                )?;

                {
                    let offscreen = surface
                        .offscreen
                        .as_mut()
                        .ok_or_else(|| anyhow!("offscreen texture missing before draw"))?;

                    let mut target = device
                        .renderer
                        .bind(&mut offscreen.texture)
                        .map_err(|e| anyhow!("bind offscreen for draw: {e}"))?;

                    let client_elements = build_output_client_elements(
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
                        &mut data.core.ui_state,
                        &data.core.scene,
                        &data.core.output_state,
                    )?;

                    let sync = frame.finish()?;
                    if let Err(e) = device.renderer.wait(&sync) {
                        flog(&format!(
                            "screenshot: GPU sync wait failed (readback may be wrong): {e}"
                        ));
                    }
                }
                //let should_capture = data.core.state.take_screenshot_request();

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
                }

                // now borrow immutably after the mutable borrow is gone
                let texture = surface
                    .offscreen
                    .as_ref()
                    .expect("offscreen texture missing")
                    .texture
                    .clone();
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
                    None,
                    None,
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
                            None,
                            None,
                            None,
                            Kind::Cursor,
                        );
                        present_elements.push(DrmPresentElement::Texture(cursor_elem));
                    }
                }

                present_elements.push(DrmPresentElement::Texture(texture_elem));

                let frame_result = surface.drm_output.render_frame(
                    &mut device.renderer,
                    &present_elements,
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                    FrameFlags::DEFAULT,
                )?;

                data.core.state.update_cursor_policy_after_drm_present(
                    &frame_result.states,
                    frame_result.cursor_element.is_some(),
                );

                if !frame_result.is_empty {
                    surface.drm_output.queue_frame(None)?;
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

                data.core.state.screenshot_all_requested = false;
            }
        }

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
        let context = EGLContext::new(&display)?;
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

    // Keep this conservative at first.
    let color_formats = [Fourcc::Argb8888, Fourcc::Xrgb8888];

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
        flog("EGL reports a software rasterizer (e.g. llvmpipe). Check drivers if you expected GPU acceleration.");
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

    let render_formats = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<Vec<_>>();

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
        [Fourcc::Argb8888],
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

            let edid_identity = connector_edid(drm_output_manager.device(), *conn)
                .and_then(|edid| parse_edid_identity(&edid));
            let make = edid_identity
                .as_ref()
                .map(|identity| identity.make.clone())
                .unwrap_or_else(|| "FlowState".to_string());
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

            let refresh_mhz = ((mode.vrefresh() as i32).max(60)) * 1000;
            let wl_mode = WlMode {
                size: (w as i32, h as i32).into(),
                refresh: refresh_mhz,
            };

            output.change_current_state(
                Some(wl_mode),
                Some(Transform::Normal),
                Some(smithay::output::Scale::Fractional(1.0)),
                Some(origin),
            );
            output.set_preferred(wl_mode);
            output.create_global::<DesktopState>(&data.core.display.handle());

            flog(&format!(
                "Wayland output advertised: name={} px={}x{} mm={}x{} refresh_mhz={} make={:?} model={:?} serial={:?}",
                output_name,
                w,
                h,
                mm_w,
                mm_h,
                wl_mode.refresh,
                make,
                model,
                serial_number,
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

            let gbm_buffer = allocator.create_buffer(
                tex_phys_size.w as u32,
                tex_phys_size.h as u32,
                Fourcc::Argb8888,
                &[DrmModifier::Linear],
            )?;

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

            //let offscreen = OffscreenOutput {
            //    size: tex_phys_size,
            //    texture: None,
            //};

            let output_id = OutputId(id);
            if let Some(out) = data.core.state.outputs.get_mut(&output_id) {
                out.handle = output.clone();
                out.physical_size = Size::<i32, Physical>::from((w as i32, h as i32));
                out.logical_size = Size::<i32, Logical>::from((w as i32, h as i32));
                out.scale_factor = 1.0;
                out.scale = smithay::utils::Scale::from((1.0, 1.0));
            }

            data.core.state.register_output_entry(
                output_id,
                output.clone(),
                origin,
                Size::<i32, Physical>::from((w as i32, h as i32)),
                1.0, // or real scale later
            );

            if !initialized_one {
                data.core.state.primary_output = output_id;
            }

            surfaces.insert(
                crtc,
                DrmSurfaceState {
                    output,
                    mode: wl_mode,
                    size: Size::<i32, Physical>::from((w as i32, h as i32)),
                    output_id: output_id,
                    origin,
                    present_render_id: Id::new(),
                    present_damage: DamageBag::default(),
                    drm_output,
                    offscreen: None,
                },
            );
            id += 1;
            next_x += w as i32;
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

    data.backend.devices.insert(node, temp_device);

    Ok(())
}
