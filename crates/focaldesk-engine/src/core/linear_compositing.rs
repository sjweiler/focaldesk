//! Shared linear-light compositing for SDR scanout (FP16 working space, sRGB KMS buffer).

use crate::core::backend_render::{draw_output, draw_output_stage, PreparedOutput};
use crate::core::color::linear_sdr_runtime_enabled;
use crate::core::desktop::DesktopState;
use crate::core::render::{ClientCompositingMode, FlowRenderElement, OutputRenderStage};
use crate::core::{OutputState, SceneState};
use anyhow::{anyhow, Context, Result};
use crate::core::ui_state::UiState;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer, Physical, Size, Transform};

pub const SDR_OFFSCREEN_FORMAT: Fourcc = Fourcc::Abgr8888;
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
        match ensure_offscreen_texture(renderer, &mut self.linear_offscreen, size, LINEAR_SDR_FORMAT)
        {
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

pub fn convert_fullscreen_texture(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &GlesTexProgram,
    label: &str,
) -> Result<()> {
    let mut target = renderer
        .bind(dst)
        .with_context(|| format!("bind target for {label}"))?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .with_context(|| format!("begin {label} frame"))?;
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (size.w as f64, size.h as f64),
    );
    let dst_rect = smithay::utils::Rectangle::<i32, Physical>::from_loc_and_size((0, 0), size);
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
            &[],
        )
        .with_context(|| format!("render {label}"))?;
    let _sync = frame
        .finish()
        .with_context(|| format!("finish {label}"))?;
    Ok(())
}

pub fn run_linear_staged_pass(
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
    srgb_to_linear: &GlesTexProgram,
    linear_to_srgb: &GlesTexProgram,
) -> Result<SyncPoint> {
    targets.ensure_offscreen(renderer, buffer_size)?;
    targets.ensure_linear_offscreen(renderer, buffer_size)?;

    let linear = targets
        .linear_offscreen
        .as_mut()
        .ok_or_else(|| anyhow!("linear offscreen missing after allocation"))?;

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
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
    }

    let sdr_texture = targets.offscreen.as_ref().unwrap().texture.clone();
    convert_fullscreen_texture(
        renderer,
        &sdr_texture,
        &mut linear.texture,
        buffer_size,
        srgb_to_linear,
        "sRGB base to linear SDR",
    )?;

    {
        let mut target = renderer
            .bind(&mut linear.texture)
            .map_err(|e| anyhow!("bind linear SDR target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin linear SDR frame: {e}"))?;
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
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
    }

    let linear_texture = targets.linear_offscreen.as_ref().unwrap().texture.clone();
    let sdr = targets
        .offscreen
        .as_mut()
        .ok_or_else(|| anyhow!("SDR offscreen missing before encode"))?;
    convert_fullscreen_texture(
        renderer,
        &linear_texture,
        &mut sdr.texture,
        buffer_size,
        linear_to_srgb,
        "linear SDR to sRGB output",
    )?;

    {
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
        )
        .map_err(|err| anyhow!("{err}"))?;
        frame.finish().map_err(Into::into)
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
