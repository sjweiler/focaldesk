//! Shared linear-light compositing for SDR scanout (FP16 working space, sRGB KMS buffer).

use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, draw_output, draw_output_stage,
    prepare_output, PreparedOutput,
};
use crate::core::color::linear_sdr_runtime_enabled;
use crate::core::desktop::DesktopState;
use crate::core::render::{
    ChromeGlassPass, ClientCompositingMode, FlowRenderElement, OutputRenderStage,
};
use crate::core::ui_state::UiState;
use crate::core::{OutputState, SceneState};
use anyhow::{anyhow, Context, Result};
use focaldesk_logging::flog;
use focaldesk_types::OutputId;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture,
};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use std::time::{Duration, Instant};

/// Opaque offscreen: avoids alpha=0 holes that the sRGB↔linear blit decodes as black.
pub const SDR_OFFSCREEN_FORMAT: Fourcc = Fourcc::Xbgr8888;
/// Alpha FP16 so unset pixels stay transparent for selective composite onto SDR.
pub const LINEAR_SDR_FORMAT: Fourcc = Fourcc::Abgr16161616f;

#[derive(Clone)]
pub struct OffscreenTexture {
    pub size: Size<i32, Physical>,
    pub texture: GlesTexture,
}

#[derive(Default)]
pub struct LinearOffscreenTargets {
    pub linear_supported: bool,
    pub offscreen: Option<OffscreenTexture>,
    pub linear_offscreen: Option<OffscreenTexture>,
}

pub fn supports_linear_sdr(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> bool {
    let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
    <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, LINEAR_SDR_FORMAT, tex_size)
        .is_ok()
}

pub fn use_linear_sdr_path(
    renderer: &mut GlesRenderer,
    targets: &LinearOffscreenTargets,
    size: Size<i32, Physical>,
) -> bool {
    linear_sdr_runtime_enabled() && targets.linear_supported && supports_linear_sdr(renderer, size)
}

impl LinearOffscreenTargets {
    pub fn offscreen_size(&self) -> Size<i32, Physical> {
        self.offscreen
            .as_ref()
            .map(|target| target.size)
            .unwrap_or_else(|| Size::from((0, 0)))
    }

    pub fn ensure_offscreen(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        ensure_offscreen_texture(renderer, &mut self.offscreen, size, SDR_OFFSCREEN_FORMAT)
    }

    pub fn ensure_linear_offscreen(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        if !self.linear_supported {
            return Ok(());
        }
        match ensure_offscreen_texture(
            renderer,
            &mut self.linear_offscreen,
            size,
            LINEAR_SDR_FORMAT,
        ) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.linear_supported = false;
                self.linear_offscreen = None;
                Err(err)
            }
        }
    }
}

pub fn ensure_offscreen_texture(
    renderer: &mut GlesRenderer,
    offscreen: &mut Option<OffscreenTexture>,
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
        *offscreen = Some(OffscreenTexture { size, texture });
    }

    Ok(())
}

/// Composite a linear FP16 client layer onto an existing SDR offscreen.
/// Pixels with alpha≈0 are discarded so wallpaper/chrome from the SDR base pass remain intact.
pub fn composite_linear_layer_onto_sdr(
    renderer: &mut GlesRenderer,
    linear: &GlesTexture,
    sdr: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &GlesTexProgram,
) -> Result<()> {
    let mut target = renderer
        .bind(sdr)
        .context("bind SDR target for linear composite")?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .context("begin linear composite frame")?;
    let tex_size = linear.size();
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (tex_size.w as f64, tex_size.h as f64),
    );
    let dst_rect = smithay::utils::Rectangle::<i32, Physical>::from_loc_and_size((0, 0), size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            linear,
            src_rect,
            dst_rect,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(shader),
            &[],
        )
        .context("render linear composite")?;
    let _sync = frame.finish().context("finish linear composite")?;
    Ok(())
}

fn clear_offscreen(
    frame: &mut GlesFrame<'_, '_>,
    size: Size<i32, Physical>,
    color: Color32F,
) -> Result<(), GlesError> {
    let full = Rectangle::from_loc_and_size((0, 0), size);
    frame.clear(color, std::slice::from_ref(&full))
}

pub fn run_linear_staged_pass(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    buffer_size: Size<i32, Physical>,
    prepared: &mut PreparedOutput,
    client_elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    srgb_to_linear: &GlesTexProgram,
    composite_linear_layer: &GlesTexProgram,
) -> Result<SyncPoint> {
    // Staged passes rewrite the full offscreen each frame; partial damage leaves
    // unscissored regions black on scanout when KMS presents with damage clips.
    prepared.frame_ctx.damage = vec![Rectangle::from_loc_and_size((0, 0), buffer_size)];

    targets.ensure_offscreen(renderer, buffer_size)?;
    targets.ensure_linear_offscreen(renderer, buffer_size)?;

    let bg = state.theme.active_theme().background.color;
    let clear_color = Color32F::new(bg[0], bg[1], bg[2], bg[3]);
    let transparent = Color32F::new(0.0, 0.0, 0.0, 0.0);

    {
        let sdr = targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("SDR offscreen missing before base draw"))?;
        let mut target = renderer
            .bind(&mut sdr.texture)
            .map_err(|e| anyhow!("bind SDR base target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin SDR base frame: {e}"))?;
        clear_offscreen(&mut frame, buffer_size, clear_color)
            .map_err(|e| anyhow!("clear SDR base target: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            client_elements,
            popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Base,
            ClientCompositingMode::Sdr,
            ChromeGlassPass::InBaseSdr,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
    }

    {
        let linear = targets
            .linear_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("linear offscreen missing after allocation"))?;
        let mut target = renderer
            .bind(&mut linear.texture)
            .map_err(|e| anyhow!("bind linear SDR target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin linear SDR frame: {e}"))?;
        clear_offscreen(&mut frame, buffer_size, transparent)
            .map_err(|e| anyhow!("clear linear client layer: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            client_elements,
            popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Clients,
            ClientCompositingMode::Linear {
                srgb_to_linear: srgb_to_linear.clone(),
            },
            ChromeGlassPass::Skip,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
    }

    {
        let linear_texture = targets.linear_offscreen.as_ref().unwrap().texture.clone();
        let sdr = targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("SDR offscreen missing before composite"))?;
        composite_linear_layer_onto_sdr(
            renderer,
            &linear_texture,
            &mut sdr.texture,
            buffer_size,
            composite_linear_layer,
        )?;
    }

    {
        let sdr = targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("SDR offscreen missing before overlay"))?;
        let mut target = renderer
            .bind(&mut sdr.texture)
            .map_err(|e| anyhow!("bind SDR overlay target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin SDR overlay frame: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            client_elements,
            popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Overlay,
            ClientCompositingMode::Sdr,
            ChromeGlassPass::Skip,
        )
        .map_err(|err| anyhow!("{err}"))?;
        frame.finish().map_err(Into::into)
    }
}

/// Render one output into the SDR offscreen target using the same linear/legacy path as scanout.
pub fn render_output_offscreen(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    now: Instant,
    dt: Duration,
    portal_capture: bool,
) -> Result<SyncPoint> {
    let srgb_to_linear = state.render.chrome_shaders.srgb_to_linear.clone();
    let composite_linear_layer = state.render.chrome_shaders.composite_linear_layer.clone();
    let use_linear = use_linear_sdr_path(renderer, targets, buffer_size)
        && srgb_to_linear.is_some()
        && composite_linear_layer.is_some();

    if use_linear {
        if let Err(err) = targets.ensure_linear_offscreen(renderer, buffer_size) {
            flog(&format!(
                "Linear SDR disabled for offscreen render after FP16 allocation failed: {err}"
            ));
        }
    }

    let client_elements = build_output_client_elements(state, renderer, output_id);
    let popup_elements = build_output_popup_elements(state, renderer, output_id);
    let mut prepared = prepare_output(
        state,
        renderer,
        output_id,
        buffer_size,
        ui_state,
        now,
        dt,
        portal_capture,
    )
    .map_err(|err| anyhow!("{err}"))?;

    if use_linear && targets.linear_offscreen.is_some() {
        run_linear_staged_pass(
            state,
            renderer,
            targets,
            buffer_size,
            &mut prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
            srgb_to_linear.as_ref().unwrap(),
            composite_linear_layer.as_ref().unwrap(),
        )
    } else {
        run_sdr_pass(
            state,
            renderer,
            targets,
            buffer_size,
            &prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
        )
    }
}

pub fn run_sdr_pass(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    buffer_size: Size<i32, Physical>,
    prepared: &PreparedOutput,
    client_elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
) -> Result<SyncPoint> {
    targets.ensure_offscreen(renderer, buffer_size)?;

    let sdr = targets
        .offscreen
        .as_mut()
        .ok_or_else(|| anyhow!("offscreen texture missing before draw"))?;
    let mut target = renderer
        .bind(&mut sdr.texture)
        .map_err(|e| anyhow!("bind offscreen for draw: {e}"))?;
    let mut frame = renderer
        .render(&mut target, buffer_size, Transform::Normal)
        .map_err(|e| anyhow!("begin offscreen frame: {e}"))?;
    draw_output(
        state,
        &mut frame,
        prepared,
        client_elements,
        popup_elements,
        ui_state,
        scene,
        output_state,
    )
    .map_err(|err| anyhow!("{err}"))?;
    frame.finish().map_err(Into::into)
}

pub fn present_offscreen_texture(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    texture: &GlesTexture,
    buffer_size: Size<i32, Physical>,
) -> Result<(), GlesError> {
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (buffer_size.w as f64, buffer_size.h as f64),
    );
    let dst_rect =
        smithay::utils::Rectangle::<i32, Physical>::from_loc_and_size((0, 0), buffer_size);
    let damage = [dst_rect];
    frame.render_texture_from_to(
        texture,
        src_rect,
        dst_rect,
        &damage,
        &damage,
        Transform::Normal,
        1.0,
        None,
        &[],
    )
}
