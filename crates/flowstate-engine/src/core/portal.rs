//! Context for completing [`ext-image-copy-capture-v1`](https://wayland.app/protocols/ext-image-copy-capture-v1)
//! frames while the GLES renderer is current (needed for `xdg-desktop-portal-wlr` and similar).
//!
//! Backends call [`DesktopState::begin_portal_dispatch`] before
//! [`wayland_server::Display::dispatch_clients`] and [`DesktopState::end_portal_dispatch`] after.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use flowstate_logging::flog;
use flowstate_types::OutputId;
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

use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, draw_output, prepare_output,
};
use crate::core::desktop::DesktopState;
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
/// On DRM the frame is queued and completed in [`complete_pending_portal_captures`] after the
/// offscreen draw so OBS receives the same pixels as the monitor.
pub fn try_render_portal_frame(state: &mut DesktopState, frame: Frame, output_id: OutputId) {
    if state.portal_dispatch_ctx.is_none() {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }

    if state.backend_kind == flowstate_flow::keybinds::BackendKind::Drm {
        state.pending_portal_captures.push(PendingPortalCapture { output_id, frame });
        return;
    }

    render_portal_frame_now(state, frame, output_id);
}

fn render_portal_frame_now(state: &mut DesktopState, frame: Frame, output_id: OutputId) {
    let Some(ctx) = state.portal_dispatch_ctx.as_mut() else {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    };

    // SAFETY: `ctx` is only populated around `dispatch_clients` and cleared immediately after.
    let renderer = unsafe { &mut *ctx.renderer.as_ptr() };
    let ui_state = unsafe { &mut *ctx.ui_state.as_ptr() };
    let scene = unsafe { &*ctx.scene.as_ptr() };
    let output_state = unsafe { &*ctx.output_state.as_ptr() };
    let now = ctx.now;
    let dt = ctx.dt;

    complete_portal_frame(
        state,
        renderer,
        ui_state,
        scene,
        output_state,
        output_id,
        frame,
        now,
        dt,
    );
}

/// Finish portal frames queued during `dispatch_clients` using the latest DRM offscreen texture.
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

    if buffer_size.w != desk_output.physical_size.w || buffer_size.h != desk_output.physical_size.h
    {
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
        flowstate_flow::keybinds::BackendKind::Winit => Transform::Flipped180,
        _ => Transform::Normal,
    };

    if let Ok(mut dmabuf) = get_dmabuf(&buffer).cloned() {
        if let Some(node) = state.dmabuf_node {
            dmabuf.set_node(node);
        }
        let render_size = Size::<i32, Physical>::from((buffer_size.w, buffer_size.h));
        let render_res = if let Some(source) = state.portal_capture_source.get(&output_id) {
            if source.size == render_size {
                blit_offscreen_source_to_dmabuf(
                    renderer,
                    source.texture.clone(),
                    render_size,
                    transform,
                    &mut dmabuf,
                )
            } else {
                render_portal_output_to_dmabuf(
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
                    &mut dmabuf,
                )
            }
        } else {
            render_portal_output_to_dmabuf(
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
                &mut dmabuf,
            )
        };
        if let Err(err) = render_res {
            flog(&format!("portal dmabuf capture render failed: {err:?}"));
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
                flog(&format!("portal shm readback failed: {err}"));
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
            flog(&format!("portal shm capture render failed: {err:?}"));
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }

        let region =
            Rectangle::<i32, Buffer>::from_loc_and_size(Point::from((0, 0)), buffer_size);
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
    let mut target = renderer.bind(target_texture)?;
    let prepared = prepare_output(state, renderer, output_id, render_size, ui_state, now, dt, true)?;
    let client_elements = build_output_client_elements(state, renderer, output_id);
    let popup_elements = build_output_popup_elements(state, renderer, output_id);
    let mut gles_frame = renderer.render(&mut target, render_size, transform)?;
    draw_output(
        state,
        &mut gles_frame,
        &prepared,
        &client_elements,
        &popup_elements,
        ui_state,
        scene,
        output_state,
    )?;
    let sync = gles_frame.finish()?;
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
    blit_texture_to_dmabuf(
        renderer,
        capture_tex,
        render_size,
        transform,
        target_dmabuf,
    )
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
    blit_texture_to_dmabuf(
        renderer,
        imported,
        render_size,
        transform,
        target_dmabuf,
    )
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

/// Append [`wlr-layer-shell`](https://wayland.app/protocols/wlr-layer-shell-unstable-v1) surfaces
/// for `output` into `out` (overlay-style: drawn after regular toplevels).
pub fn push_layer_elements_for_output(
    renderer: &mut GlesRenderer,
    output: &smithay::output::Output,
    origin: Point<i32, smithay::utils::Logical>,
    logical_size: Size<i32, smithay::utils::Logical>,
    output_scale: smithay::utils::Scale<f64>,
    out: &mut Vec<crate::core::render::FlowRenderElement>,
) {
    use smithay::backend::renderer::element::AsRenderElements;
    use smithay::utils::Logical;

    let output_rect = Rectangle::<i32, Logical>::from_loc_and_size(origin, logical_size);
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        let Some(geo) = map.layer_geometry(layer) else {
            continue;
        };
        if !geo.overlaps(output_rect) {
            continue;
        }
        let local_loc = geo.loc - origin;
        let render_pos = local_loc.to_physical_precise_round(output_scale);
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

/// Store the Wayland [`Output`](smithay::output::Output) on an image-capture source (for constraints).
pub fn attach_output_to_capture_source(
    source: &smithay::wayland::image_capture_source::ImageCaptureSource,
    output: &smithay::output::Output,
) {
    source.user_data().insert_if_missing(|| output.downgrade());
}
