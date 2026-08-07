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
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, ImportMem, Offscreen, Renderer};
use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::image_copy_capture::{CaptureFailureReason, Frame};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut};

use crate::core::desktop::DesktopState;
use crate::core::linear_compositing::{
    present_offscreen_texture, render_output_offscreen, select_hdr_offscreen_format,
    supports_linear_sdr, LinearOffscreenTargets,
};
use crate::core::scene::SceneState;
use crate::core::ui_state::UiState;
use crate::core::OutputState;
use smithay::backend::renderer::Frame as RendererFrame;
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::SessionRef;

const PORTAL_CAPTURE_MIN_INTERVAL: Duration = Duration::from_millis(66);

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

/// Portal frame received during `dispatch_clients`; completed after the DRM offscreen draw.
pub struct PendingPortalCapture {
    pub output_id: OutputId,
    pub frame: Frame,
}

/// Last DRM offscreen frame per output — portal blits this instead of re-rendering so OBS
/// matches what the monitor shows (sidebar/topbar chrome included).
pub struct PortalCaptureSource {
    pub texture: GlesTexture,
    pub size: Size<i32, Physical>,
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

/// Store the latest scanout offscreen texture for portal/OBS clients on this output.
pub fn publish_portal_capture_source(
    state: &mut DesktopState,
    output_id: OutputId,
    texture: GlesTexture,
    size: Size<i32, Physical>,
    captured_at: Instant,
) {
    state.portal_capture_source.insert(
        output_id,
        PortalCaptureSource {
            texture,
            size,
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
/// Frames are queued and completed after the offscreen draw so OBS receives the same pixels
/// as the monitor (including linear SDR compositing when enabled).
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

/// True when every queued portal frame can be satisfied from the latest scanout texture.
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

/// Finish portal frames for one output immediately after its offscreen draw (same pixels as monitor).
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
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer a GPU blit into the portal dmabuf. The readback path is a fallback for drivers
    // where `render_texture_from_to` drops chrome when sourcing from an FBO texture handle.
    if source_size == target_size {
        match blit_texture_to_dmabuf(
            renderer,
            texture.clone(),
            source_size,
            transform,
            target_dmabuf,
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
            source_size,
            target_size,
            transform,
            target_dmabuf,
        ) {
            Ok(()) => return Ok(()),
            Err(err) => flog(format!(
                "portal dmabuf GPU scaled blit failed ({err}); falling back to readback"
            )),
        }
    }

    let mut bound = texture;
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
        source_size,
        target_size,
        transform,
        target_dmabuf,
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

    if let Ok(mut dmabuf) = get_dmabuf(&buffer).cloned() {
        if let Some(node) = state.dmabuf_node {
            dmabuf.set_node(node);
        }
        let target_size = stream_size;
        let render_res = if let Some(source) = state.portal_capture_source.get(&output_id) {
            blit_offscreen_source_to_dmabuf_scaled(
                renderer,
                source.texture.clone(),
                source.size,
                target_size,
                transform,
                &mut dmabuf,
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
        if source.size != render_size {
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
        let mut tex = source.texture.clone();
        match read_bound_offscreen_rgba(renderer, &mut tex, render_size) {
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
) -> Result<(), Box<dyn std::error::Error>> {
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

    let offscreen = targets
        .scanout_texture()
        .ok_or("portal offscreen missing after render")?
        .clone();
    store_portal_offscreen_targets(state, output_id, targets);

    let mut target = renderer.bind(target_texture)?;
    let mut frame = renderer.render(&mut target, render_size, transform)?;
    present_offscreen_texture(&mut frame, &offscreen, render_size)?;
    let sync = frame.finish()?;
    renderer.wait(&sync)?;
    Ok(())
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
) -> Result<(), Box<dyn std::error::Error>> {
    // Render to an intermediate texture (same path as the DRM monitor), then blit into the
    // portal dmabuf. Direct draws into client-imported dmabufs miss chrome on some drivers.
    let mut capture_tex = renderer.create_buffer(
        Fourcc::Abgr8888,
        Size::<i32, Buffer>::from((render_size.w, render_size.h)),
    )?;
    render_portal_output_to_texture(
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
    )?;
    blit_texture_to_dmabuf(renderer, capture_tex, render_size, transform, target_dmabuf)
}

/// Copy the DRM offscreen texture into a portal dmabuf using the same FBO readback path as
/// internal screenshots. `render_texture_from_to` from the texture handle alone drops chrome on
/// some GLES stacks; bound-FBO readback matches what you see on the monitor.
fn blit_offscreen_source_to_dmabuf(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    render_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut bound = texture;
    let rgba = read_bound_offscreen_rgba(renderer, &mut bound, render_size)?;
    let imported = renderer.import_memory(
        &rgba,
        Fourcc::Abgr8888,
        Size::from((render_size.w, render_size.h)),
        false,
    )?;
    blit_texture_to_dmabuf(renderer, imported, render_size, transform, target_dmabuf)
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
    render_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = renderer.bind(target_dmabuf)?;
    let mut frame = renderer.render(&mut target, render_size, transform)?;
    let dest = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), render_size);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (render_size.w as f64, render_size.h as f64),
    );
    frame.render_texture_from_to(
        &texture,
        src,
        dest,
        std::slice::from_ref(&dest),
        &[],
        transform,
        1.0,
        None,
        &[],
    )?;
    let sync = frame.finish()?;
    renderer.wait(&sync)?;
    Ok(())
}

fn blit_texture_to_dmabuf_scaled(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
    transform: Transform,
    target_dmabuf: &mut Dmabuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut target = renderer.bind(target_dmabuf)?;
    let mut frame = renderer.render(&mut target, target_size, transform)?;
    let dest = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), target_size);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (source_size.w as f64, source_size.h as f64),
    );
    frame.render_texture_from_to(
        &texture,
        src,
        dest,
        std::slice::from_ref(&dest),
        &[],
        transform,
        1.0,
        None,
        &[],
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
