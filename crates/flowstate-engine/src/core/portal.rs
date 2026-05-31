//! Context for completing [`ext-image-copy-capture-v1`](https://wayland.app/protocols/ext-image-copy-capture-v1)
//! frames while the GLES renderer is current (needed for `xdg-desktop-portal-wlr` and similar).
//!
//! Backends call [`DesktopState::begin_portal_dispatch`] before
//! [`wayland_server::Display::dispatch_clients`] and [`DesktopState::end_portal_dispatch`] after.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use flowstate_types::OutputId;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, Offscreen, Renderer};
use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::image_copy_capture::{CaptureFailureReason, Frame};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut};

use crate::core::backend_render::{build_output_client_elements, draw_output, prepare_output};
use crate::core::desktop::DesktopState;
use crate::core::scene::SceneState;
use crate::core::ui_state::UiState;
use crate::core::OutputState;
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::FlowThemeId;
use flowstate_themes::ThemeManager;
use smithay::backend::renderer::Frame as RendererFrame;
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::SessionRef;

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

/// Renders the active output into the portal client's SHM buffer, if dispatch context is set.
pub fn try_render_portal_frame(state: &mut DesktopState, frame: Frame, output_id: OutputId) {
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

    let buffer = frame.buffer();
    let buffer_size = match with_buffer_contents(&buffer, |_, _, data| {
        Size::<i32, BufferCoords>::from((data.width, data.height))
    }) {
        Ok(s) => s,
        Err(_) => {
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

    let mut capture_tex = match <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
        renderer,
        Fourcc::Argb8888,
        buffer_size,
    ) {
        Ok(t) => t,
        Err(_) => {
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
    };

    let transform = match state.backend_kind {
        flowstate_flow::keybinds::BackendKind::Winit => Transform::Flipped180,
        _ => Transform::Normal,
    };

    let render_res = (|| -> Result<(), Box<dyn std::error::Error>> {
        let render_size = Size::<i32, Physical>::from((buffer_size.w, buffer_size.h));
        let prepared = prepare_output(state, renderer, output_id, render_size, ui_state, now, dt)?;

        {
            let mut target = renderer.bind(&mut capture_tex)?;
            let client_elements = build_output_client_elements(state, renderer, output_id);
            let mut gles_frame = renderer.render(&mut target, render_size, transform)?;
            let mut theme_manager = ThemeManager::new(FlowThemeId::BuiltIn(BuiltInThemeId::Eagle));
            let theme = theme_manager.active_theme();
            draw_output(
                state,
                &mut gles_frame,
                &prepared,
                &client_elements,
                ui_state,
                scene,
                output_state,
            )?;

            let sync = gles_frame.finish()?;
            renderer.wait(&sync)?;
        }
        Ok(())
    })();

    if render_res.is_err() {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }

    let width = buffer_size.w as usize;
    let height = buffer_size.h as usize;
    let mut rgba = vec![0u8; width * height * 4];
    let region =
        Rectangle::<i32, BufferCoords>::from_loc_and_size(Point::from((0, 0)), buffer_size);
    let read_res = (|| -> Result<(), smithay::backend::renderer::gles::GlesError> {
        // Match `create_buffer(Fourcc::Argb8888)` (`GL_BGRA8_EXT`): read as BGRA then swizzle to RGBA
        // for the SHM conversion loop below (it expects `rgba[..]` in R,G,B,A order per pixel).
        let mapping = renderer.copy_texture(&capture_tex, region, Fourcc::Argb8888)?;
        let src = renderer.map_texture(&mapping)?;
        rgba.copy_from_slice(src);
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(())
    })();

    if read_res.is_err() {
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }

    let write_res = with_buffer_contents_mut(&buffer, |ptr, len, data| -> Result<(), ()> {
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
    });

    if write_res.is_err() {
        frame.fail(CaptureFailureReason::BufferConstraints);
        return;
    }

    frame.success(
        transform,
        None::<Vec<Rectangle<i32, BufferCoords>>>,
        std::time::Duration::ZERO,
    );
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
