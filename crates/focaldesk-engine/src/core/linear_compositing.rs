//! Shared linear-light compositing for SDR scanout (FP16 working space, sRGB KMS buffer).

use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, draw_output, draw_output_stage,
    prepare_output, PreparedOutput,
};
use crate::core::color::{
    kms_scanout_encode_description, linear_sdr_runtime_enabled, output_encode_scanout_needed,
    scene_to_output_matrix, RenderingIntent,
};
use crate::core::icc_lut::icc_lut_shader_enabled;
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
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use std::time::{Duration, Instant};

/// Opaque offscreen: wallpaper/chrome in sRGB, KMS scanout target.
pub const SDR_OFFSCREEN_FORMAT: Fourcc = Fourcc::Xbgr8888;
/// Alpha FP16 client layer composited onto the SDR base.
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
    /// Ping-pong target for full-frame output encode (C1b).
    pub encode_scratch: Option<OffscreenTexture>,
    pub encoded_scanout: bool,
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

    pub fn ensure_encode_scratch(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        ensure_offscreen_texture(renderer, &mut self.encode_scratch, size, SDR_OFFSCREEN_FORMAT)
    }

    /// Texture to present or capture after the render pass (encode scratch when C1b ran).
    pub fn scanout_texture(&self) -> Option<&GlesTexture> {
        if self.encoded_scanout {
            self.encode_scratch.as_ref().map(|t| &t.texture)
        } else {
            self.offscreen.as_ref().map(|t| &t.texture)
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

/// Composite the FP16 client layer onto an existing SDR base using linear→sRGB only.
/// Transparent pixels are discarded so the base pass remains visible.
pub fn composite_linear_layer_onto_sdr_srgb(
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
    let damage = [Rectangle::from_loc_and_size((0, 0), size)];
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

/// Full-frame scene sRGB → monitor encode (C1b parametric or C2c ICC LUT).
pub fn apply_output_encode(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
) -> Result<Option<SyncPoint>> {
    let output_encode =
        kms_scanout_encode_description(state.output_color_description(output_id));
    let lut_owned = state
        .outputs
        .get(&output_id)
        .and_then(|output| output.output_icc_lut.clone());
    if !output_encode_scanout_needed(output_encode, lut_owned.as_ref()) {
        return Ok(None);
    }

    let lut_shader = state.render.chrome_shaders.output_encode_lut.clone();
    if lut_owned.is_some() && icc_lut_shader_enabled() {
        if let Some(shader) = lut_shader.as_ref() {
            return apply_output_encode_lut(
                state,
                renderer,
                targets,
                output_id,
                buffer_size,
                lut_owned.as_ref().unwrap(),
                shader,
            );
        }
    }

    let parametric_shader = state
        .render
        .chrome_shaders
        .output_encode_sdr
        .clone()
        .ok_or_else(|| anyhow!("output encode shader missing"))?;
    apply_output_encode_parametric(
        state,
        renderer,
        targets,
        output_id,
        buffer_size,
        output_encode,
        &parametric_shader,
    )
}

fn apply_output_encode_lut(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    lut: &crate::core::icc_lut::OutputIccLut,
    shader: &GlesTexProgram,
) -> Result<Option<SyncPoint>> {
    let lut_texture = state
        .render
        .ensure_output_icc_lut_texture(renderer, output_id, lut)
        .map_err(|e| anyhow!("upload ICC LUT atlas: {e}"))?;

    let scene_texture = targets
        .offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("SDR offscreen missing before output encode"))?
        .texture
        .clone();
    targets.ensure_encode_scratch(renderer, buffer_size)?;
    let scratch = targets
        .encode_scratch
        .as_mut()
        .ok_or_else(|| anyhow!("encode scratch missing after allocation"))?;

    let grid = lut.grid_size as f32;
    let uniforms = vec![
        Uniform::new("u_lut_tex", 1i32),
        Uniform::new("u_grid", grid),
    ];

    let mut target = renderer
        .bind(&mut scratch.texture)
        .map_err(|e| anyhow!("bind encode scratch: {e}"))?;
    let mut frame = renderer
        .render(&mut target, buffer_size, Transform::Normal)
        .map_err(|e| anyhow!("begin output encode frame: {e}"))?;

    let lut_tex_id = lut_texture.tex_id();
    frame
        .with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, lut_tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::NEAREST as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::NEAREST as i32);
        })
        .map_err(|e| anyhow!("bind ICC LUT texture: {e}"))?;

    let tex_size = scene_texture.size();
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (tex_size.w as f64, tex_size.h as f64),
    );
    let dst_rect = Rectangle::from_loc_and_size((0, 0), buffer_size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            &scene_texture,
            src_rect,
            dst_rect,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(shader),
            &uniforms,
        )
        .map_err(|e| anyhow!("render ICC LUT output encode: {e}"))?;
    let sync = frame
        .finish()
        .map_err(|e| anyhow!("finish ICC LUT output encode: {e}"))?;
    targets.encoded_scanout = true;
    Ok(Some(sync))
}

fn apply_output_encode_parametric(
    _state: &DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    _output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    output_encode: crate::core::color::ColorDescription,
    shader: &GlesTexProgram,
) -> Result<Option<SyncPoint>> {
    let scene_texture = targets
        .offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("SDR offscreen missing before output encode"))?
        .texture
        .clone();
    targets.ensure_encode_scratch(renderer, buffer_size)?;
    let scratch = targets
        .encode_scratch
        .as_mut()
        .ok_or_else(|| anyhow!("encode scratch missing after allocation"))?;

    use smithay::backend::renderer::gles::Uniform;

    let scene_to_output = scene_to_output_matrix(output_encode, RenderingIntent::Relative);
    let encode_tf = output_encode.transfer.encode_mode() as u32 as f32;
    let uniforms = vec![
        Uniform::new("u_encode_tf", encode_tf),
        Uniform::new("u_m0", [
            scene_to_output[0][0],
            scene_to_output[0][1],
            scene_to_output[0][2],
        ]),
        Uniform::new("u_m1", [
            scene_to_output[1][0],
            scene_to_output[1][1],
            scene_to_output[1][2],
        ]),
        Uniform::new("u_m2", [
            scene_to_output[2][0],
            scene_to_output[2][1],
            scene_to_output[2][2],
        ]),
    ];

    let mut target = renderer
        .bind(&mut scratch.texture)
        .map_err(|e| anyhow!("bind encode scratch: {e}"))?;
    let mut frame = renderer
        .render(&mut target, buffer_size, Transform::Normal)
        .map_err(|e| anyhow!("begin output encode frame: {e}"))?;
    let tex_size = scene_texture.size();
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (tex_size.w as f64, tex_size.h as f64),
    );
    let dst_rect = Rectangle::from_loc_and_size((0, 0), buffer_size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            &scene_texture,
            src_rect,
            dst_rect,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(shader),
            &uniforms,
        )
        .map_err(|e| anyhow!("render output encode: {e}"))?;
    let sync = frame.finish().map_err(|e| anyhow!("finish output encode: {e}"))?;
    targets.encoded_scanout = true;
    Ok(Some(sync))
}

fn finish_with_output_encode(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    sync: SyncPoint,
) -> Result<SyncPoint> {
    if let Some(encode_sync) =
        apply_output_encode(state, renderer, targets, output_id, buffer_size)?
    {
        return Ok(encode_sync);
    }
    Ok(sync)
}

/// Composite the FP16 client layer onto an existing SDR base (wallpaper/chrome).
/// Transparent pixels are discarded so the base pass remains visible.
pub fn composite_linear_layer_onto_sdr(
    renderer: &mut GlesRenderer,
    linear: &GlesTexture,
    sdr: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &GlesTexProgram,
    scene_to_output: [[f32; 3]; 3],
    encode_tf: f32,
) -> Result<()> {
    use smithay::backend::renderer::gles::Uniform;

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
    let damage = [Rectangle::from_loc_and_size((0, 0), size)];
    let uniforms = vec![
        Uniform::new("u_encode_tf", encode_tf),
        Uniform::new("u_m0", [
            scene_to_output[0][0],
            scene_to_output[0][1],
            scene_to_output[0][2],
        ]),
        Uniform::new("u_m1", [
            scene_to_output[1][0],
            scene_to_output[1][1],
            scene_to_output[1][2],
        ]),
        Uniform::new("u_m2", [
            scene_to_output[2][0],
            scene_to_output[2][1],
            scene_to_output[2][2],
        ]),
    ];
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
            &uniforms,
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
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    prepared: &mut PreparedOutput,
    client_elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    client_to_scene: &GlesTexProgram,
    linear_to_srgb: &GlesTexProgram,
) -> Result<SyncPoint> {
    targets.encoded_scanout = false;
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
            ChromeGlassPass::Skip,
            false,
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
            OutputRenderStage::LinearGlassUnderClients,
            ClientCompositingMode::Sdr,
            ChromeGlassPass::LinearUnderClients,
            true,
        )
        .map_err(|err| anyhow!("{err}"))?;
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
                client_to_scene: client_to_scene.clone(),
            },
            ChromeGlassPass::Skip,
            true,
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
        composite_linear_layer_onto_sdr_srgb(
            renderer,
            &linear_texture,
            &mut sdr.texture,
            buffer_size,
            linear_to_srgb,
        )?;
    }

    let sync = {
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
            true,
        )
        .map_err(|err| anyhow!("{err}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            client_elements,
            popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::EguiOverlay,
            ClientCompositingMode::Sdr,
            ChromeGlassPass::Skip,
            false,
        )
        .map_err(|err| anyhow!("{err}"))?;
        frame
            .finish()
            .map_err(|e| anyhow!("finish SDR overlay frame: {e}"))?
    };
    finish_with_output_encode(
        state,
        renderer,
        targets,
        output_id,
        buffer_size,
        sync,
    )
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
    let client_to_scene = state.render.chrome_shaders.client_to_scene_linear.clone();
    let linear_to_srgb = state.render.chrome_shaders.linear_to_srgb.clone();
    let use_linear = use_linear_sdr_path(renderer, targets, buffer_size)
        && client_to_scene.is_some()
        && linear_to_srgb.is_some();

    if use_linear {
        if let Err(err) = targets.ensure_linear_offscreen(renderer, buffer_size) {
            flog(format!(
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
            output_id,
            buffer_size,
            &mut prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
            client_to_scene.as_ref().unwrap(),
            linear_to_srgb.as_ref().unwrap(),
        )
    } else {
        run_sdr_pass(
            state,
            renderer,
            targets,
            output_id,
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
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    prepared: &PreparedOutput,
    client_elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
) -> Result<SyncPoint> {
    targets.encoded_scanout = false;
    targets.ensure_offscreen(renderer, buffer_size)?;

    let sync = {
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
        frame
            .finish()
            .map_err(|e| anyhow!("finish offscreen frame: {e}"))?
    };
    finish_with_output_encode(state, renderer, targets, output_id, buffer_size, sync)
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
