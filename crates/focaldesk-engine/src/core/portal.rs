//! Context for completing [`ext-image-copy-capture-v1`](https://wayland.app/protocols/ext-image-copy-capture-v1)
//! frames while the GLES renderer is current (needed for `xdg-desktop-portal-wlr` and similar).
//!
//! Backends call [`DesktopState::begin_portal_dispatch`] before
//! [`wayland_server::Display::dispatch_clients`] and [`DesktopState::end_portal_dispatch`] after.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use focaldesk_logging::flog;
use focaldesk_types::OutputId;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Buffer as AllocatorBuffer;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexProgram, GlesTexture, Uniform};
use smithay::backend::renderer::{Bind, ExportMem, ImportMem, Offscreen, Renderer};
use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::image_copy_capture::{CaptureFailureReason, Frame};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut};

use crate::core::desktop::DesktopState;
use crate::core::linear_compositing::{
    render_output_offscreen, select_hdr_offscreen_format, supports_linear_sdr,
    LinearOffscreenTargets,
};
use crate::core::scene::SceneState;
use crate::core::ui_state::UiState;
use crate::core::OutputState;
use smithay::backend::renderer::Frame as RendererFrame;
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::SessionRef;

const PORTAL_CAPTURE_MIN_INTERVAL: Duration = Duration::from_millis(66);

/// Color contract for untagged `ext-image-copy-capture-v1` streams consumed by
/// xdg-desktop-portal-wlr and PipeWire clients such as OBS.
pub const PORTAL_CAPTURE_COLOR: crate::core::color::ColorDescription =
    crate::core::color::ColorDescription::SRGB;

/// Pointers to objects that must be live for the duration of `dispatch_clients` only.
#[derive(Clone, Copy)]
pub struct PortalDispatchCtx {
    pub renderer: NonNull<GlesRenderer>,
    pub ui_state: NonNull<UiState<smithay::backend::renderer::gles::GlesTexture>>,
    pub scene: NonNull<SceneState>,
    pub output_state: NonNull<OutputState>,
    pub now: Instant,
    pub dt: Duration,
}

pub struct PortalFrameCache {
    pub size: Size<i32, Buffer>,
    pub rgba: Vec<u8>,
    pub transform: Transform,
    pub captured_at: Instant,
}

/// Color interpretation of the texture exported to a portal capture client.
///
/// `ext-image-copy-capture-v1` negotiates pixel formats but has no color-space
/// metadata. FocalDesk therefore treats every current portal stream as
/// sRGB/Rec.709 and converts the canonical scene before handing pixels to the
/// portal. This enum keeps that contract explicit and leaves room for a future
/// tagged wide-gamut/HDR transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalCaptureEncoding {
    /// Linear-light Rec.709 scene values, potentially outside the SDR gamut.
    LinearRec709,
    /// Already encoded for the untagged sRGB/Rec.709 portal contract.
    Srgb,
}

#[derive(Clone, Copy)]
struct PortalCaptureTransform<'a> {
    shader: Option<&'a GlesTexProgram>,
    source_peak: f32,
}

fn portal_capture_source_peak(output: &crate::core::desktop::OutputState) -> f32 {
    let hdr_scene = output.hdr_enabled || output.hdr_transition_target == Some(true);
    portal_capture_peak(
        hdr_scene,
        output.color_description.reference_white_nits,
        output.edid_hdr_max_luminance_nits,
        output.color_description.max_luminance_nits,
    )
}

fn portal_capture_peak(
    hdr_scene: bool,
    reference_white_nits: f32,
    metadata_peak_nits: Option<f32>,
    described_peak_nits: f32,
) -> f32 {
    if !hdr_scene {
        return 1.0;
    }
    let reference_white = reference_white_nits.max(1.0);
    let source_peak_nits = metadata_peak_nits
        .filter(|peak| peak.is_finite() && *peak > reference_white)
        .unwrap_or(described_peak_nits);
    source_peak_nits.max(reference_white) / reference_white
}

fn tone_map_luminance(value: f32, source_peak: f32) -> f32 {
    const KNEE: f32 = 0.75;
    if source_peak <= 1.0 || value <= KNEE {
        return value;
    }
    let peak = source_peak.max(KNEE + 0.0001);
    let denominator = 1.0 - (-(peak - KNEE) / (1.0 - KNEE)).exp();
    let numerator = 1.0 - (-(value - KNEE) / (1.0 - KNEE)).exp();
    KNEE + (1.0 - KNEE) * numerator / denominator.max(0.0001)
}

/// Portal frame received during `dispatch_clients`; completed after the DRM offscreen draw.
pub struct PendingPortalCapture {
    pub output_id: OutputId,
    pub frame: Frame,
}

/// Last composited frame per output before the monitor-specific color encode.
/// Portal capture reuses it so OBS receives the same scene content (including
/// sidebar/topbar chrome) under the portal color contract.
pub struct PortalCaptureSource {
    pub texture: GlesTexture,
    pub size: Size<i32, Physical>,
    pub encoding: PortalCaptureEncoding,
    pub captured_at: Instant,
}

impl DesktopState {
    pub fn begin_portal_dispatch(
        &mut self,
        renderer: &mut GlesRenderer,
        ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
        scene: &mut SceneState,
        output_state: &OutputState,
        now: Instant,
        dt: Duration,
    ) {
        self.portal_dispatch_ctx = Some(PortalDispatchCtx {
            renderer: NonNull::from(renderer),
            ui_state: NonNull::from(ui_state),
            scene: NonNull::from(scene),
            output_state: NonNull::from(output_state),
            now,
            dt,
        });
    }

    pub fn end_portal_dispatch(&mut self) {
        self.portal_dispatch_ctx = None;
    }
}

/// Store the latest pre-output-transform texture for portal/OBS clients.
pub fn publish_portal_capture_source(
    state: &mut DesktopState,
    output_id: OutputId,
    texture: GlesTexture,
    size: Size<i32, Physical>,
    encoding: PortalCaptureEncoding,
    captured_at: Instant,
) {
    state.portal_capture_source.insert(
        output_id,
        PortalCaptureSource {
            texture,
            size,
            encoding,
            captured_at,
        },
    );
    state.compositor_ready = true;
}

fn portal_offscreen_targets_for_output(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    size: Size<i32, Physical>,
) -> LinearOffscreenTargets {
    let mut targets = state
        .portal_offscreen_targets
        .remove(&output_id)
        .unwrap_or_else(|| {
            let hdr_format = select_hdr_offscreen_format(renderer, size);
            LinearOffscreenTargets {
                linear_supported: supports_linear_sdr(renderer, size),
                hdr_supported: hdr_format.is_some(),
                hdr_format,
                ..LinearOffscreenTargets::default()
            }
        });
    if targets.offscreen_size() != size {
        targets.offscreen = None;
        targets.linear_offscreen = None;
        targets.hdr_offscreen = None;
        targets.encode_scratch = None;
    }
    if !targets.linear_supported {
        targets.linear_supported = supports_linear_sdr(renderer, size);
    }
    if !targets.hdr_supported {
        targets.hdr_format = select_hdr_offscreen_format(renderer, size);
        targets.hdr_supported = targets.hdr_format.is_some();
    }
    targets
}

fn store_portal_offscreen_targets(
    state: &mut DesktopState,
    output_id: OutputId,
    targets: LinearOffscreenTargets,
) {
    state.portal_offscreen_targets.insert(output_id, targets);
}

/// Select the compositor image before the monitor-specific output transform.
///
/// The FP16 scene is preferred because it retains wide-gamut precision. The
/// legacy fallback is the compositor's original sRGB target, not the encoded
/// scanout texture, so an ICC/P3 monitor transform is never mislabeled as sRGB.
pub fn portal_source_from_targets(
    targets: &LinearOffscreenTargets,
) -> Option<(GlesTexture, PortalCaptureEncoding)> {
    if targets.scene_linear {
        targets
            .linear_offscreen
            .as_ref()
            .map(|target| (target.texture.clone(), PortalCaptureEncoding::LinearRec709))
    } else {
        targets
            .offscreen
            .as_ref()
            .map(|target| (target.texture.clone(), PortalCaptureEncoding::Srgb))
    }
}

pub fn output_id_for_session(state: &DesktopState, session: &SessionRef) -> Option<OutputId> {
    use smithay::output::WeakOutput;

    let source: ImageCaptureSource = session.source();
    let weak_output = source.user_data().get::<WeakOutput>()?;
    let output = weak_output.upgrade()?;

    state.outputs.iter().find_map(|(id, out)| {
        if out.handle == output {
            Some(*id)
        } else {
            None
        }
    })
}

/// Renders the active output into the portal client's buffer, if dispatch context is set.
///
/// Frames are queued and completed after the offscreen draw so OBS receives the
/// same composited scene as the monitor, converted to the portal color contract.
pub fn try_render_portal_frame(state: &mut DesktopState, frame: Frame, output_id: OutputId) {
    if state.portal_dispatch_ctx.is_none() {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }

    state
        .pending_portal_captures
        .push(PendingPortalCapture { output_id, frame });
}

/// Fail in-flight frames and discard GPU resources indexed by the old OutputIds.
/// A DRM topology rebuild may immediately reuse those numeric IDs for other monitors.
pub(crate) fn invalidate_portal_output_state(state: &mut DesktopState) {
    for pending in state.pending_portal_captures.drain(..) {
        pending.frame.fail(CaptureFailureReason::Unknown);
    }
    state.portal_frame_cache.clear();
    state.portal_capture_source.clear();
    state.portal_offscreen_targets.clear();
    state.compositor_ready = false;
}

/// True when every queued portal frame can be satisfied from the latest composited texture.
pub fn pending_portal_outputs_have_capture_source(state: &DesktopState) -> bool {
    state
        .pending_portal_captures
        .iter()
        .all(|cap| state.portal_capture_source.contains_key(&cap.output_id))
}

/// Portal frames waiting to be completed after `dispatch_clients`.
pub fn portal_capture_pending(state: &DesktopState) -> bool {
    !state.pending_portal_captures.is_empty()
}

/// Whether portal capture requires a fresh compositor draw this frame.
pub fn portal_needs_composite(state: &DesktopState) -> bool {
    portal_capture_pending(state)
        && (!state.compositor_ready || !pending_portal_outputs_have_capture_source(state))
}

/// Finish portal frames queued during `dispatch_clients` using the latest offscreen texture.
pub fn complete_pending_portal_captures(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    now: Instant,
    dt: Duration,
) {
    if state.pending_portal_captures.is_empty() {
        return;
    }

    let pending = std::mem::take(&mut state.pending_portal_captures);
    for cap in pending {
        complete_portal_frame(
            state,
            renderer,
            ui_state,
            scene,
            output_state,
            cap.output_id,
            cap.frame,
            now,
            dt,
        );
    }
}

/// Finish portal frames for one output immediately after its composited offscreen draw.
pub fn complete_pending_portal_captures_for_output(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    output_id: OutputId,
    now: Instant,
    dt: Duration,
) {
    if state.pending_portal_captures.is_empty() {
        return;
    }

    let (for_output, rest): (Vec<_>, Vec<_>) = state
        .pending_portal_captures
        .drain(..)
        .partition(|cap| cap.output_id == output_id);
    state.pending_portal_captures = rest;

    for cap in for_output {
        complete_portal_frame(
            state,
            renderer,
            ui_state,
            scene,
            output_state,
            cap.output_id,
            cap.frame,
            now,
            dt,
        );
    }
}

fn blit_offscreen_source_to_dmabuf_scaled(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    encoding: PortalCaptureEncoding,
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer a GPU blit into the portal dmabuf. The readback path is a fallback for drivers
    // where `render_texture_from_to` drops chrome when sourcing from an FBO texture handle.
    if source_size == target_size {
        match blit_texture_to_dmabuf(
            renderer,
            texture.clone(),
            encoding,
            source_size,
            transform,
            target_dmabuf,
            capture_transform,
        ) {
            Ok(()) => return Ok(()),
            Err(err) => flog(format!(
                "portal dmabuf GPU blit failed ({err}); falling back to readback"
            )),
        }
    } else {
        match blit_texture_to_dmabuf_scaled(
            renderer,
            texture.clone(),
            encoding,
            source_size,
            target_size,
            transform,
            target_dmabuf,
            capture_transform,
        ) {
            Ok(()) => return Ok(()),
            Err(err) => flog(format!(
                "portal dmabuf GPU scaled blit failed ({err}); falling back to readback"
            )),
        }
    }

    // Some GLES stacks cannot sample an FBO texture directly while rendering
    // into an imported dmabuf. Materialize the sRGB contract first so this
    // compatibility path never leaks linear or monitor-encoded RGB values.
    let mut encoded = renderer.create_buffer(
        Fourcc::Abgr8888,
        Size::<i32, Buffer>::from((source_size.w, source_size.h)),
    )?;
    blit_capture_texture_to_texture(
        renderer,
        texture,
        encoding,
        source_size,
        source_size,
        Transform::Normal,
        &mut encoded,
        capture_transform,
    )?;
    let mut bound = encoded;
    let rgba = read_bound_offscreen_rgba(renderer, &mut bound, source_size)?;
    let imported = renderer.import_memory(
        &rgba,
        Fourcc::Abgr8888,
        Size::from((source_size.w, source_size.h)),
        false,
    )?;
    blit_texture_to_dmabuf_scaled(
        renderer,
        imported,
        PortalCaptureEncoding::Srgb,
        source_size,
        target_size,
        transform,
        target_dmabuf,
        capture_transform,
    )
}

fn complete_portal_frame(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    output_id: OutputId,
    frame: Frame,
    now: Instant,
    dt: Duration,
) {
    let buffer = frame.buffer();
    let buffer_size = match capture_buffer_size(&buffer) {
        Some(size) => size,
        None => {
            frame.fail(CaptureFailureReason::BufferConstraints);
            return;
        }
    };

    let desk_output = match state.outputs.get(&output_id) {
        Some(o) => o,
        None => {
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
    };

    let output_size = desk_output.physical_size; // real monitor size, e.g. 2560x1440
    let source_peak = portal_capture_source_peak(desk_output);
    let stream_size = Size::<i32, Physical>::from((buffer_size.w, buffer_size.h)); // OBS, e.g. 2048x1152

    if buffer_size.w <= 0 || buffer_size.h <= 0 {
        frame.fail(CaptureFailureReason::BufferConstraints);
        return;
    }

    if !state.compositor_ready {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }

    // Frame cache only applies to SHM readback; dmabuf clients must receive a fresh GPU render.
    if get_dmabuf(&buffer).is_err() {
        if let Some(cache) = state.portal_frame_cache.get(&output_id) {
            if cache.size == buffer_size
                && now.duration_since(cache.captured_at) < PORTAL_CAPTURE_MIN_INTERVAL
            {
                if write_rgba_to_shm_buffer(&buffer, buffer_size, &cache.rgba).is_ok() {
                    frame.success(
                        cache.transform,
                        None::<Vec<Rectangle<i32, Buffer>>>,
                        Duration::ZERO,
                    );
                } else {
                    frame.fail(CaptureFailureReason::BufferConstraints);
                }
                return;
            }
        }
    }

    let transform = match state.backend_kind {
        focaldesk_flow::keybinds::BackendKind::Winit => Transform::Flipped180,
        _ => Transform::Normal,
    };
    let capture_shader = state.render.chrome_shaders.portal_capture_sdr.clone();
    let capture_transform = PortalCaptureTransform {
        shader: capture_shader.as_ref(),
        source_peak,
    };

    if let Ok(mut dmabuf) = get_dmabuf(&buffer).cloned() {
        if let Some(node) = state.dmabuf_node {
            dmabuf.set_node(node);
        }
        let target_size = stream_size;
        let render_res = if let Some(source) = state.portal_capture_source.get(&output_id) {
            blit_offscreen_source_to_dmabuf_scaled(
                renderer,
                source.texture.clone(),
                source.encoding,
                source.size,
                target_size,
                transform,
                &mut dmabuf,
                capture_transform,
            )
        } else {
            render_portal_output_to_dmabuf(
                state,
                renderer,
                output_id,
                output_size,
                ui_state,
                scene,
                output_state,
                now,
                dt,
                transform,
                &mut dmabuf,
                capture_transform,
            )
        };
        if let Err(err) = render_res {
            flog(format!("portal dmabuf capture render failed: {err:?}"));
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }

        frame.success(
            transform,
            None::<Vec<Rectangle<i32, Buffer>>>,
            std::time::Duration::ZERO,
        );
        return;
    }

    let render_size = Size::<i32, Physical>::from((buffer_size.w, buffer_size.h));
    let rgba = if let Some(source) = state.portal_capture_source.get(&output_id) {
        let mut capture_tex = match renderer.create_buffer(
            Fourcc::Abgr8888,
            Size::<i32, Buffer>::from((buffer_size.w, buffer_size.h)),
        ) {
            Ok(texture) => texture,
            Err(_) => {
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
        };
        if let Err(err) = blit_capture_texture_to_texture(
            renderer,
            source.texture.clone(),
            source.encoding,
            source.size,
            render_size,
            Transform::Normal,
            &mut capture_tex,
            capture_transform,
        ) {
            flog(format!("portal shm color conversion failed: {err}"));
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
        match read_bound_offscreen_rgba(renderer, &mut capture_tex, render_size) {
            Ok(pixels) => pixels,
            Err(err) => {
                flog(format!("portal shm readback failed: {err}"));
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
        }
    } else {
        let mut capture_tex = match renderer.create_buffer(
            Fourcc::Abgr8888,
            Size::<i32, Buffer>::from((buffer_size.w, buffer_size.h)),
        ) {
            Ok(t) => t,
            Err(_) => {
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
        };

        let render_res = render_portal_output_to_texture(
            state,
            renderer,
            output_id,
            render_size,
            ui_state,
            scene,
            output_state,
            now,
            dt,
            transform,
            &mut capture_tex,
            capture_transform,
        );

        if let Err(err) = render_res {
            flog(format!("portal shm capture render failed: {err:?}"));
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }

        let region = Rectangle::<i32, Buffer>::from_loc_and_size(Point::from((0, 0)), buffer_size);
        match (|| -> Result<Vec<u8>, smithay::backend::renderer::gles::GlesError> {
            let mapping = renderer.copy_texture(&capture_tex, region, Fourcc::Abgr8888)?;
            let src = renderer.map_texture(&mapping)?;
            Ok(src.to_vec())
        })() {
            Ok(pixels) => pixels,
            Err(_) => {
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
        }
    };

    state.portal_frame_cache.insert(
        output_id,
        PortalFrameCache {
            size: buffer_size,
            rgba: rgba.clone(),
            transform,
            captured_at: now,
        },
    );

    let write_res = write_rgba_to_shm_buffer(&buffer, buffer_size, &rgba);

    if write_res.is_err() {
        frame.fail(CaptureFailureReason::BufferConstraints);
        return;
    }

    frame.success(
        transform,
        None::<Vec<Rectangle<i32, Buffer>>>,
        std::time::Duration::ZERO,
    );
}

fn capture_buffer_size(
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
) -> Option<Size<i32, Buffer>> {
    with_buffer_contents(buffer, |_, _, data| {
        Size::<i32, Buffer>::from((data.width, data.height))
    })
    .ok()
    .or_else(|| get_dmabuf(buffer).ok().map(|dmabuf| dmabuf.size()))
}

fn render_portal_output_to_texture(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    render_size: Size<i32, Physical>,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    now: Instant,
    dt: Duration,
    transform: Transform,
    target_texture: &mut GlesTexture,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (source, encoding) = render_fresh_portal_source(
        state,
        renderer,
        output_id,
        render_size,
        ui_state,
        scene,
        output_state,
        now,
        dt,
    )?;
    blit_capture_texture_to_texture(
        renderer,
        source,
        encoding,
        render_size,
        render_size,
        transform,
        target_texture,
        capture_transform,
    )
}

fn render_fresh_portal_source(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    render_size: Size<i32, Physical>,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    now: Instant,
    dt: Duration,
) -> Result<(GlesTexture, PortalCaptureEncoding), Box<dyn std::error::Error>> {
    let mut targets = portal_offscreen_targets_for_output(state, renderer, output_id, render_size);
    let sync = render_output_offscreen(
        state,
        renderer,
        &mut targets,
        output_id,
        render_size,
        ui_state,
        scene,
        output_state,
        now,
        dt,
        true,
    )?;
    renderer.wait(&sync)?;

    let source =
        portal_source_from_targets(&targets).ok_or("portal capture source missing after render")?;
    store_portal_offscreen_targets(state, output_id, targets);
    Ok(source)
}

fn render_portal_output_to_dmabuf(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    render_size: Size<i32, Physical>,
    ui_state: &mut UiState<smithay::backend::renderer::gles::GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    now: Instant,
    dt: Duration,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (source, encoding) = render_fresh_portal_source(
        state,
        renderer,
        output_id,
        render_size,
        ui_state,
        scene,
        output_state,
        now,
        dt,
    )?;
    blit_texture_to_dmabuf(
        renderer,
        source,
        encoding,
        render_size,
        transform,
        target_dmabuf,
        capture_transform,
    )
}

fn portal_capture_program<'a>(
    encoding: PortalCaptureEncoding,
    capture_transform: PortalCaptureTransform<'a>,
) -> Result<(Option<&'a GlesTexProgram>, Vec<Uniform<'static>>), Box<dyn std::error::Error>> {
    if encoding == PortalCaptureEncoding::Srgb {
        return Ok((None, Vec::new()));
    }

    let shader = capture_transform
        .shader
        .ok_or("tone-mapped linear-to-sRGB portal shader unavailable")?;
    let description = PORTAL_CAPTURE_COLOR;
    let matrix = crate::core::color::scene_to_output_matrix(
        description,
        crate::core::color::RenderingIntent::Relative,
    );
    Ok((
        Some(shader),
        vec![
            Uniform::new("u_source_peak", capture_transform.source_peak.max(1.0)),
            Uniform::new("u_m0", matrix[0]),
            Uniform::new("u_m1", matrix[1]),
            Uniform::new("u_m2", matrix[2]),
        ],
    ))
}

fn blit_capture_texture_to_texture(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    encoding: PortalCaptureEncoding,
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
    transform: Transform,
    target_texture: &mut GlesTexture,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = renderer.bind(target_texture)?;
    let mut frame = renderer.render(&mut target, target_size, transform)?;
    let dest = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), target_size);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (source_size.w as f64, source_size.h as f64),
    );
    let (shader, uniforms) = portal_capture_program(encoding, capture_transform)?;
    frame.render_texture_from_to(
        &texture,
        src,
        dest,
        std::slice::from_ref(&dest),
        &[],
        transform,
        1.0,
        shader,
        &uniforms,
    )?;
    let sync = frame.finish()?;
    renderer.wait(&sync)?;
    Ok(())
}

fn read_bound_offscreen_rgba(
    renderer: &mut GlesRenderer,
    texture: &mut GlesTexture,
    render_size: Size<i32, Physical>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use smithay::backend::renderer::gles::ffi;

    let target = renderer.bind(texture)?;
    let region = Rectangle::<i32, Buffer>::from_loc_and_size(
        Point::from((0, 0)),
        Size::from((render_size.w, render_size.h)),
    );

    renderer
        .with_context(|gl| unsafe {
            gl.BindBuffer(ffi::PIXEL_PACK_BUFFER, 0);
        })
        .map_err(|e| format!("portal readback GL state: {e}"))?;

    let mapping = renderer
        .copy_framebuffer(&target, region, Fourcc::Abgr8888)
        .map_err(|e| format!("portal copy_framebuffer: {e}"))?;
    renderer
        .with_context(|gl| unsafe {
            gl.Finish();
        })
        .map_err(|e| format!("portal readback Finish: {e}"))?;
    Ok(renderer
        .map_texture(&mapping)
        .map_err(|e| format!("portal map_texture: {e}"))?
        .to_vec())
}

fn blit_texture_to_dmabuf(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    encoding: PortalCaptureEncoding,
    render_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = renderer.bind(target_dmabuf)?;
    let mut frame = renderer.render(&mut target, render_size, transform)?;
    let dest = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), render_size);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (render_size.w as f64, render_size.h as f64),
    );
    let (shader, uniforms) = portal_capture_program(encoding, capture_transform)?;
    frame.render_texture_from_to(
        &texture,
        src,
        dest,
        std::slice::from_ref(&dest),
        &[],
        transform,
        1.0,
        shader,
        &uniforms,
    )?;
    let sync = frame.finish()?;
    renderer.wait(&sync)?;
    Ok(())
}

fn blit_texture_to_dmabuf_scaled(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    encoding: PortalCaptureEncoding,
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
    capture_transform: PortalCaptureTransform<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = renderer.bind(target_dmabuf)?;
    let mut frame = renderer.render(&mut target, target_size, transform)?;
    let dest = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), target_size);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (source_size.w as f64, source_size.h as f64),
    );
    let (shader, uniforms) = portal_capture_program(encoding, capture_transform)?;
    frame.render_texture_from_to(
        &texture,
        src,
        dest,
        std::slice::from_ref(&dest),
        &[],
        transform,
        1.0,
        shader,
        &uniforms,
    )?;
    let sync = frame.finish()?;
    renderer.wait(&sync)?;
    Ok(())
}

fn write_rgba_to_shm_buffer(
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    buffer_size: Size<i32, Buffer>,
    rgba: &[u8],
) -> Result<(), ()> {
    let width = buffer_size.w as usize;
    let height = buffer_size.h as usize;

    if rgba.len() != width.saturating_mul(height).saturating_mul(4) {
        return Err(());
    }

    with_buffer_contents_mut(buffer, |ptr, len, data| -> Result<(), ()> {
        if data.height != buffer_size.h || data.width != buffer_size.w {
            return Err(());
        }
        let format = data.format;
        let stride = data.stride as usize;
        if stride.saturating_mul(height) > len {
            return Err(());
        }
        // SAFETY: slice only lives for this write; client must not race writes (portal protocol).
        let mmap = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        match format {
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => {
                for y in 0..height {
                    let src_y = height - 1 - y;
                    for x in 0..width {
                        let si = (src_y * width + x) * 4;
                        let di = y * stride + x * 4;
                        let r = rgba[si];
                        let g = rgba[si + 1];
                        let b = rgba[si + 2];
                        let a = if format == wl_shm::Format::Xrgb8888 {
                            255
                        } else {
                            rgba[si + 3]
                        };
                        mmap[di] = b;
                        mmap[di + 1] = g;
                        mmap[di + 2] = r;
                        mmap[di + 3] = a;
                    }
                }
                Ok(())
            }
            _ => Err(()),
        }
    })
    .map_err(|_| ())?
}

fn push_layer_elements_for_output_matching(
    renderer: &mut GlesRenderer,
    output: &smithay::output::Output,
    logical_size: Size<i32, smithay::utils::Logical>,
    output_scale: smithay::utils::Scale<f64>,
    trusted_shell: bool,
    out: &mut Vec<crate::core::render::FlowRenderElement>,
) {
    use smithay::backend::renderer::element::AsRenderElements;
    use smithay::utils::Logical;

    // LayerMap geometry is output-local. Using the Space/global output origin
    // here made every layer on a non-origin output fail the overlap test and,
    // if it survived, shifted its render position by the output origin again.
    let output_rect = Rectangle::<i32, Logical>::from_size(logical_size);
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        if crate::core::wayland::trusted_shell::is_trusted_namespace(layer.namespace())
            != trusted_shell
        {
            continue;
        }
        let Some(geo) = map.layer_geometry(layer) else {
            continue;
        };
        if !geo.overlaps(output_rect) {
            continue;
        }
        let render_pos = geo.loc.to_physical_precise_round(output_scale);
        out.extend(
            layer.render_elements::<crate::core::render::FlowRenderElement>(
                renderer,
                render_pos,
                output_scale,
                1.0,
            ),
        );
    }
}

/// Append ordinary layer-shell surfaces to the color-managed client pass.
pub fn push_layer_elements_for_output(
    renderer: &mut GlesRenderer,
    output: &smithay::output::Output,
    logical_size: Size<i32, smithay::utils::Logical>,
    output_scale: smithay::utils::Scale<f64>,
    out: &mut Vec<crate::core::render::FlowRenderElement>,
) {
    push_layer_elements_for_output_matching(
        renderer,
        output,
        logical_size,
        output_scale,
        false,
        out,
    );
}

/// Append FocalDesk's trusted panel and dock to the final sRGB UI pass.
///
/// Their GLES shaders use the same sRGB theme values as compositor-owned
/// chrome. Keeping them out of the scene-linear application pass preserves
/// the appearance of translucent bevels and glass controls.
pub fn push_trusted_shell_elements_for_output(
    renderer: &mut GlesRenderer,
    output: &smithay::output::Output,
    logical_size: Size<i32, smithay::utils::Logical>,
    output_scale: smithay::utils::Scale<f64>,
    out: &mut Vec<crate::core::render::FlowRenderElement>,
) {
    push_layer_elements_for_output_matching(
        renderer,
        output,
        logical_size,
        output_scale,
        true,
        out,
    );
}

/// Store the Wayland [`Output`](smithay::output::Output) on an image-capture source (for constraints).
pub fn attach_output_to_capture_source(
    source: &smithay::wayland::image_capture_source::ImageCaptureSource,
    output: &smithay::output::Output,
) {
    source.user_data().insert_if_missing(|| output.downgrade());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::{ColorPrimaries, TransferFunction};

    #[test]
    fn untagged_portal_contract_is_sdr_srgb() {
        assert_eq!(PORTAL_CAPTURE_COLOR.primaries, ColorPrimaries::Srgb);
        assert_eq!(PORTAL_CAPTURE_COLOR.transfer, TransferFunction::Srgb);
        assert_eq!(PORTAL_CAPTURE_COLOR.reference_white_nits, 80.0);
        assert_eq!(PORTAL_CAPTURE_COLOR.max_luminance_nits, 80.0);
    }

    #[test]
    fn sdr_capture_does_not_apply_a_tone_curve() {
        assert_eq!(
            portal_capture_peak(false, 80.0, Some(1_000.0), 1_000.0),
            1.0
        );
        for value in [0.0, 0.18, 0.75, 1.0] {
            assert_eq!(tone_map_luminance(value, 1.0), value);
        }
    }

    #[test]
    fn hdr_capture_preserves_diffuse_values_and_rolls_highlights_into_sdr() {
        let peak = portal_capture_peak(true, 80.0, Some(800.0), 1_000.0);
        assert_eq!(peak, 10.0);
        assert_eq!(tone_map_luminance(0.75, peak), 0.75);

        let white = tone_map_luminance(1.0, peak);
        let highlight = tone_map_luminance(4.0, peak);
        let peak_value = tone_map_luminance(peak, peak);
        assert!(white > 0.75 && white < highlight);
        assert!(highlight < peak_value);
        assert!((peak_value - 1.0).abs() < 0.0001);
    }

    #[test]
    fn hdr_capture_peak_falls_back_to_the_color_description() {
        assert_eq!(portal_capture_peak(true, 200.0, None, 1_000.0), 5.0);
        assert_eq!(portal_capture_peak(true, 200.0, Some(100.0), 1_000.0), 5.0);
    }
}
