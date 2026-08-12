//! Shared linear-light compositing for SDR scanout (FP16 working space, sRGB KMS buffer)
//! and HDR PQ scanout (C3b: scRGB working space, 10-bit PQ offscreen).

use crate::core::backend_render::{
    build_output_client_elements, build_output_popup_elements, draw_output, draw_output_stage,
    prepare_output, PreparedOutput,
};
use crate::core::color::{
    hdr_render_runtime_enabled, kms_scanout_encode_description, linear_sdr_runtime_enabled,
    output_encode_scanout_needed, scene_to_output_matrix, RenderingIntent,
};
use crate::core::desktop::DesktopState;
use crate::core::icc_lut::icc_lut_shader_enabled;
use crate::core::render::{
    ChromeGlassPass, ClientCompositingMode, FlowRenderElement, OutputRenderStage,
};
use crate::core::ui_state::UiState;
use crate::core::{OutputState, SceneState};
use anyhow::{anyhow, Context, Result};
use focaldesk_logging::{flog, flog_warn};
use focaldesk_types::OutputId;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use std::time::{Duration, Instant};

/// Opaque encoded SDR intermediate used by the legacy path and ICC LUT pass.
pub const SDR_OFFSCREEN_FORMAT: Fourcc = Fourcc::Xbgr8888;
/// Transparent sRGB overlay used for egui and the software cursor before they
/// are decoded back into the scene-linear FP16 target.
pub const SDR_OVERLAY_FORMAT: Fourcc = Fourcc::Abgr8888;
/// Full scene-linear Rec.709 compositor target. Extended and negative channel
/// values remain representable until the final output-gamut conversion.
pub const LINEAR_SDR_FORMAT: Fourcc = Fourcc::Abgr16161616f;
/// PQ-encoded HDR scanout (C3b).
pub const HDR_OFFSCREEN_FORMATS: [Fourcc; 2] = [Fourcc::Abgr2101010, Fourcc::Argb2101010];

#[derive(Clone)]
pub struct OffscreenTexture {
    pub size: Size<i32, Physical>,
    pub texture: GlesTexture,
}

#[derive(Default)]
pub struct LinearOffscreenTargets {
    pub linear_supported: bool,
    pub hdr_supported: bool,
    pub hdr_format: Option<Fourcc>,
    pub offscreen: Option<OffscreenTexture>,
    pub linear_offscreen: Option<OffscreenTexture>,
    pub overlay_offscreen: Option<OffscreenTexture>,
    /// Ping-pong target for full-frame output encode (C1b/C2c SDR).
    pub encode_scratch: Option<OffscreenTexture>,
    /// PQ-encoded HDR scanout buffer (C3b).
    pub hdr_offscreen: Option<OffscreenTexture>,
    pub encoded_scanout: bool,
    pub encoded_hdr: bool,
    /// The current frame in `linear_offscreen` is the canonical composited scene.
    pub scene_linear: bool,
}

pub fn supports_linear_sdr(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> bool {
    let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
    <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, LINEAR_SDR_FORMAT, tex_size)
        .is_ok()
}

pub fn select_hdr_offscreen_format(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Option<Fourcc> {
    let tex_size = Size::<i32, Buffer>::from((size.w, size.h));
    HDR_OFFSCREEN_FORMATS.iter().copied().find(|format| {
        <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, *format, tex_size).is_ok()
    })
}

pub fn supports_hdr_offscreen(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> bool {
    select_hdr_offscreen_format(renderer, size).is_some()
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

    pub fn ensure_overlay_offscreen(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        ensure_offscreen_texture(
            renderer,
            &mut self.overlay_offscreen,
            size,
            SDR_OVERLAY_FORMAT,
        )
    }

    pub fn ensure_encode_scratch(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        ensure_offscreen_texture(
            renderer,
            &mut self.encode_scratch,
            size,
            SDR_OFFSCREEN_FORMAT,
        )
    }

    pub fn ensure_hdr_offscreen(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(), GlesError> {
        let format = match self.hdr_format {
            Some(format) => format,
            None => {
                let Some(format) = select_hdr_offscreen_format(renderer, size) else {
                    self.hdr_supported = false;
                    return Ok(());
                };
                self.hdr_format = Some(format);
                self.hdr_supported = true;
                format
            }
        };
        ensure_offscreen_texture(renderer, &mut self.hdr_offscreen, size, format)
    }

    /// Texture to present or capture after the render pass (HDR PQ or SDR encode scratch).
    pub fn scanout_texture(&self) -> Option<&GlesTexture> {
        if self.encoded_scanout {
            if self.encoded_hdr {
                return self.hdr_offscreen.as_ref().map(|t| &t.texture);
            }
            return self.encode_scratch.as_ref().map(|t| &t.texture);
        }
        self.offscreen.as_ref().map(|t| &t.texture)
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

/// Decode a premultiplied sRGB overlay and blend it into the FP16 scene.
fn composite_srgb_overlay_onto_linear(
    renderer: &mut GlesRenderer,
    overlay: &GlesTexture,
    linear_scene: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &GlesTexProgram,
) -> Result<()> {
    let mut target = renderer
        .bind(linear_scene)
        .context("bind linear scene for sRGB overlay")?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .context("begin sRGB overlay composite")?;
    let tex_size = overlay.size();
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (tex_size.w as f64, tex_size.h as f64),
    );
    let dst_rect = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            overlay,
            src_rect,
            dst_rect,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(shader),
            &[],
        )
        .context("decode and composite sRGB overlay")?;
    let _sync = frame.finish().context("finish sRGB overlay composite")?;
    Ok(())
}

/// Full-frame scene sRGB → monitor encode (C1b parametric, C2c ICC LUT, or C3b HDR PQ).
fn resolve_hdr_encode_state(
    selected: bool,
    requested: bool,
    supported: bool,
    kms_applied: bool,
    transition_target: Option<bool>,
    test_encode: bool,
) -> (bool, bool) {
    let kms_target = selected && requested && supported && transition_target.unwrap_or(kms_applied);
    (kms_target || (selected && test_encode), kms_target)
}

pub fn apply_output_encode(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
) -> Result<Option<SyncPoint>> {
    targets.encoded_hdr = false;
    let output_state = state.outputs.get(&output_id);
    let (hdr_active, hdr_kms_target) = output_state
        .map(|o| {
            let selected = crate::core::color::hdr_output_selected(&o.handle.name());
            let test_encode = selected
                && crate::core::color::output_hdr_pq_test_encode_active(
                    o.hdr_requested,
                    o.hdr_supported,
                    o.hdr_kms_applied,
                );
            resolve_hdr_encode_state(
                selected,
                o.hdr_requested,
                o.hdr_supported,
                o.hdr_kms_applied,
                o.hdr_transition_target,
                test_encode,
            )
        })
        .unwrap_or((false, false));
    let hdr_max = output_state.and_then(|o| o.edid_hdr_max_luminance_nits);
    let hdr_fall = output_state.and_then(|o| o.edid_hdr_max_fall_nits);
    let sdr_white_nits = output_state
        .map(|o| o.color_description.reference_white_nits)
        .unwrap_or(80.0);

    // A real KMS HDR transition must carry a PQ-encoded first frame. The
    // runtime flag remains relevant only to the userspace-only PQ lab mode.
    if hdr_active && (hdr_kms_target || hdr_render_runtime_enabled()) {
        if let Some(max_nits) = hdr_max.filter(|n| *n > 0.0) {
            if targets.hdr_supported {
                match apply_hdr_pq_encode(
                    state,
                    renderer,
                    targets,
                    buffer_size,
                    max_nits,
                    sdr_white_nits,
                ) {
                    Ok(sync) => return Ok(sync),
                    Err(err) => {
                        flog_warn!(
                            "HDR PQ encode failed for {:?}: {err}; falling back to SDR encode",
                            output_id
                        );
                    }
                }
            } else {
                flog_warn!(
                    "HDR render requested for {:?} but 10-bit offscreen is unavailable; using SDR encode",
                    output_id
                );
            }
        } else {
            flog_warn!(
                "HDR render requested for {:?} but EDID max luminance is missing; using SDR encode",
                output_id
            );
        }
    }

    let output_encode = kms_scanout_encode_description(
        state.output_color_description(output_id),
        false,
        hdr_max,
        hdr_fall,
    );
    let lut_owned = state
        .outputs
        .get(&output_id)
        .and_then(|output| output.output_icc_lut.clone());
    if !output_encode_scanout_needed(output_encode, lut_owned.as_ref()) {
        return Ok(None);
    }

    let lut_shader = state.render.chrome_shaders.output_encode_lut.clone();
    if let Some(lut) = lut_owned.as_ref() {
        if icc_lut_shader_enabled() {
            if let Some(shader) = lut_shader.as_ref() {
                match apply_output_encode_lut(
                    state,
                    renderer,
                    targets,
                    output_id,
                    buffer_size,
                    lut,
                    shader,
                ) {
                    Ok(sync) => return Ok(sync),
                    Err(err) => {
                        flog_warn!(
                            "ICC LUT output encode failed for {:?}: {err}; falling back to parametric encode",
                            output_id
                        );
                        disable_output_icc_lut(state, output_id, "ICC LUT encode failed");
                    }
                }
            } else {
                disable_output_icc_lut(state, output_id, "ICC LUT shader unavailable");
            }
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

fn disable_output_icc_lut(state: &mut DesktopState, output_id: OutputId, reason: &str) {
    if let Some(output) = state.outputs.get_mut(&output_id) {
        if output.output_icc_lut.take().is_some() {
            output.icc_lut_fallback_active = true;
            let first_notice = state.render.icc_lut_fallback_logged.insert(output_id);
            if first_notice {
                flog_warn!(
                    "ICC LUT fallback active for {:?}: {}; using parametric SDR encode until restart",
                    output_id,
                    reason
                );
            }
        }
    }
    state.notify_runtime_display_status_changes();
}

fn blit_with_shader(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    size: Size<i32, Physical>,
    shader: &GlesTexProgram,
    uniforms: &[Uniform<'_>],
    label: &str,
) -> Result<SyncPoint> {
    let mut target = renderer
        .bind(dst)
        .map_err(|e| anyhow!("bind target for {label}: {e}"))?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .map_err(|e| anyhow!("begin {label} frame: {e}"))?;
    let tex_size = src.size();
    let src_rect = smithay::utils::Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (tex_size.w as f64, tex_size.h as f64),
    );
    let dst_rect = Rectangle::from_loc_and_size((0, 0), size);
    let damage = [dst_rect];
    frame
        .render_texture_from_to(
            src,
            src_rect,
            dst_rect,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(shader),
            uniforms,
        )
        .map_err(|e| anyhow!("render {label}: {e}"))?;
    frame.finish().map_err(|e| anyhow!("finish {label}: {e}"))
}

fn apply_hdr_pq_encode(
    state: &DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    buffer_size: Size<i32, Physical>,
    max_nits: f32,
    sdr_white_nits: f32,
) -> Result<Option<SyncPoint>> {
    let sdr_to_scrgb = state
        .render
        .chrome_shaders
        .sdr_to_linear_scrgb
        .as_ref()
        .ok_or_else(|| anyhow!("sdr_to_linear_scrgb shader missing"))?;
    let scrgb_to_pq = state
        .render
        .chrome_shaders
        .linear_scrgb_to_pq
        .as_ref()
        .ok_or_else(|| anyhow!("linear_scrgb_to_pq shader missing"))?;

    let scene_texture = targets
        .offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("SDR offscreen missing before HDR PQ encode"))?
        .texture
        .clone();

    targets.ensure_linear_offscreen(renderer, buffer_size)?;
    targets.ensure_hdr_offscreen(renderer, buffer_size)?;

    let working_texture = targets
        .linear_offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("FP16 working buffer missing for HDR PQ encode"))?
        .texture
        .clone();

    let _sync = blit_with_shader(
        renderer,
        &scene_texture,
        &mut targets
            .linear_offscreen
            .as_mut()
            .expect("linear offscreen")
            .texture,
        buffer_size,
        sdr_to_scrgb,
        // Keep the legacy encoded-SDR fallback in the same scene convention as
        // the native linear path: 1.0 is reference white. The PQ pass below
        // applies the actual output reference-white luminance exactly once.
        &[Uniform::new("u_sdr_white_nits", 80.0f32)],
        "SDR-to-linear-scRGB",
    )?;

    let sync = blit_with_shader(
        renderer,
        &working_texture,
        &mut targets
            .hdr_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("HDR offscreen missing after allocation"))?
            .texture,
        buffer_size,
        scrgb_to_pq,
        &{
            let scene_to_bt2020 = scene_to_output_matrix(
                crate::core::color::ColorDescription::bt2020_pq_hdr(max_nits, max_nits),
                RenderingIntent::Relative,
            );
            [
                Uniform::new("u_max_nits", max_nits),
                Uniform::new("u_sdr_white_nits", sdr_white_nits),
                Uniform::new(
                    "u_m0",
                    [
                        scene_to_bt2020[0][0],
                        scene_to_bt2020[0][1],
                        scene_to_bt2020[0][2],
                    ],
                ),
                Uniform::new(
                    "u_m1",
                    [
                        scene_to_bt2020[1][0],
                        scene_to_bt2020[1][1],
                        scene_to_bt2020[1][2],
                    ],
                ),
                Uniform::new(
                    "u_m2",
                    [
                        scene_to_bt2020[2][0],
                        scene_to_bt2020[2][1],
                        scene_to_bt2020[2][2],
                    ],
                ),
            ]
        },
        "linear-scRGB-to-PQ",
    )?;

    targets.encoded_scanout = true;
    targets.encoded_hdr = true;
    Ok(Some(sync))
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
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_MIN_FILTER,
                ffi::NEAREST as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_MAG_FILTER,
                ffi::NEAREST as i32,
            );
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
        Uniform::new(
            "u_m0",
            [
                scene_to_output[0][0],
                scene_to_output[0][1],
                scene_to_output[0][2],
            ],
        ),
        Uniform::new(
            "u_m1",
            [
                scene_to_output[1][0],
                scene_to_output[1][1],
                scene_to_output[1][2],
            ],
        ),
        Uniform::new(
            "u_m2",
            [
                scene_to_output[2][0],
                scene_to_output[2][1],
                scene_to_output[2][2],
            ],
        ),
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
    let sync = frame
        .finish()
        .map_err(|e| anyhow!("finish output encode: {e}"))?;
    targets.encoded_scanout = true;
    Ok(Some(sync))
}

fn apply_linear_output_encode_parametric(
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    buffer_size: Size<i32, Physical>,
    output_encode: crate::core::color::ColorDescription,
    shader: &GlesTexProgram,
    intermediate_for_lut: bool,
) -> Result<SyncPoint> {
    let scene_texture = targets
        .linear_offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("FP16 scene missing before output encode"))?
        .texture
        .clone();

    if intermediate_for_lut {
        targets.ensure_offscreen(renderer, buffer_size)?;
    } else {
        targets.ensure_encode_scratch(renderer, buffer_size)?;
    }

    let scene_to_output = scene_to_output_matrix(output_encode, RenderingIntent::Relative);
    let encode_tf = output_encode.transfer.encode_mode() as u32 as f32;
    let uniforms = vec![
        Uniform::new("u_encode_tf", encode_tf),
        Uniform::new(
            "u_m0",
            [
                scene_to_output[0][0],
                scene_to_output[0][1],
                scene_to_output[0][2],
            ],
        ),
        Uniform::new(
            "u_m1",
            [
                scene_to_output[1][0],
                scene_to_output[1][1],
                scene_to_output[1][2],
            ],
        ),
        Uniform::new(
            "u_m2",
            [
                scene_to_output[2][0],
                scene_to_output[2][1],
                scene_to_output[2][2],
            ],
        ),
    ];

    let destination = if intermediate_for_lut {
        &mut targets.offscreen.as_mut().unwrap().texture
    } else {
        &mut targets.encode_scratch.as_mut().unwrap().texture
    };
    let sync = blit_with_shader(
        renderer,
        &scene_texture,
        destination,
        buffer_size,
        shader,
        &uniforms,
        "linear scene-to-output encode",
    )?;
    if !intermediate_for_lut {
        targets.encoded_scanout = true;
    }
    Ok(sync)
}

fn apply_linear_hdr_pq_encode(
    state: &DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    buffer_size: Size<i32, Physical>,
    max_nits: f32,
    sdr_white_nits: f32,
) -> Result<SyncPoint> {
    let shader = state
        .render
        .chrome_shaders
        .linear_scrgb_to_pq
        .as_ref()
        .ok_or_else(|| anyhow!("linear-scene-to-PQ shader missing"))?;
    let scene_texture = targets
        .linear_offscreen
        .as_ref()
        .ok_or_else(|| anyhow!("FP16 scene missing before HDR PQ encode"))?
        .texture
        .clone();
    targets.ensure_hdr_offscreen(renderer, buffer_size)?;
    let scene_to_bt2020 = scene_to_output_matrix(
        crate::core::color::ColorDescription::bt2020_pq_hdr(max_nits, max_nits),
        RenderingIntent::Relative,
    );
    let uniforms = vec![
        Uniform::new("u_max_nits", max_nits),
        Uniform::new("u_sdr_white_nits", sdr_white_nits),
        Uniform::new(
            "u_m0",
            [
                scene_to_bt2020[0][0],
                scene_to_bt2020[0][1],
                scene_to_bt2020[0][2],
            ],
        ),
        Uniform::new(
            "u_m1",
            [
                scene_to_bt2020[1][0],
                scene_to_bt2020[1][1],
                scene_to_bt2020[1][2],
            ],
        ),
        Uniform::new(
            "u_m2",
            [
                scene_to_bt2020[2][0],
                scene_to_bt2020[2][1],
                scene_to_bt2020[2][2],
            ],
        ),
    ];
    let destination = &mut targets
        .hdr_offscreen
        .as_mut()
        .ok_or_else(|| anyhow!("HDR offscreen unavailable after allocation"))?
        .texture;
    let sync = blit_with_shader(
        renderer,
        &scene_texture,
        destination,
        buffer_size,
        shader,
        &uniforms,
        "linear scene-to-PQ",
    )?;
    targets.encoded_scanout = true;
    targets.encoded_hdr = true;
    Ok(sync)
}

fn apply_linear_output_encode(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
) -> Result<SyncPoint> {
    targets.encoded_scanout = false;
    targets.encoded_hdr = false;
    targets.scene_linear = true;
    let output_state = state.outputs.get(&output_id);
    let (hdr_active, hdr_kms_target) = output_state
        .map(|output| {
            let selected = crate::core::color::hdr_output_selected(&output.handle.name());
            let test_encode = selected
                && crate::core::color::output_hdr_pq_test_encode_active(
                    output.hdr_requested,
                    output.hdr_supported,
                    output.hdr_kms_applied,
                );
            resolve_hdr_encode_state(
                selected,
                output.hdr_requested,
                output.hdr_supported,
                output.hdr_kms_applied,
                output.hdr_transition_target,
                test_encode,
            )
        })
        .unwrap_or((false, false));
    let hdr_max = output_state.and_then(|output| output.edid_hdr_max_luminance_nits);
    let sdr_white_nits = output_state
        .map(|output| output.color_description.reference_white_nits)
        .unwrap_or(80.0);

    if hdr_active && (hdr_kms_target || hdr_render_runtime_enabled()) {
        if let Some(max_nits) = hdr_max.filter(|nits| *nits > 0.0) {
            if targets.hdr_supported {
                match apply_linear_hdr_pq_encode(
                    state,
                    renderer,
                    targets,
                    buffer_size,
                    max_nits,
                    sdr_white_nits,
                ) {
                    Ok(sync) => return Ok(sync),
                    Err(err) => flog_warn!(
                        "linear HDR PQ encode failed for {:?}: {err}; falling back to SDR",
                        output_id
                    ),
                }
            }
        }
    }

    let output_encode = state.output_color_description(output_id);
    let lut_owned = state
        .outputs
        .get(&output_id)
        .and_then(|output| output.output_icc_lut.clone());
    let parametric_shader = state
        .render
        .chrome_shaders
        .output_encode_linear
        .clone()
        .ok_or_else(|| anyhow!("linear output encode shader missing"))?;
    let lut_shader = state.render.chrome_shaders.output_encode_lut.clone();
    let use_lut = lut_owned.is_some() && icc_lut_shader_enabled() && lut_shader.is_some();

    let mut sync = apply_linear_output_encode_parametric(
        renderer,
        targets,
        buffer_size,
        output_encode,
        &parametric_shader,
        use_lut,
    )?;
    if let (Some(lut), Some(shader)) = (lut_owned.as_ref(), lut_shader.as_ref()) {
        if use_lut {
            match apply_output_encode_lut(
                state,
                renderer,
                targets,
                output_id,
                buffer_size,
                lut,
                shader,
            ) {
                Ok(Some(lut_sync)) => sync = lut_sync,
                Ok(None) => {}
                Err(err) => {
                    flog_warn!(
                        "ICC LUT encode of linear scene failed for {:?}: {err}; using parametric output encode",
                        output_id
                    );
                    disable_output_icc_lut(state, output_id, "linear-scene ICC LUT encode failed");
                    sync = apply_linear_output_encode_parametric(
                        renderer,
                        targets,
                        buffer_size,
                        output_encode,
                        &parametric_shader,
                        false,
                    )?;
                }
            }
        }
    }
    Ok(sync)
}

fn finish_with_output_encode(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    targets: &mut LinearOffscreenTargets,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    sync: SyncPoint,
) -> Result<SyncPoint> {
    match apply_output_encode(state, renderer, targets, output_id, buffer_size) {
        Ok(Some(encode_sync)) => Ok(encode_sync),
        Ok(None) => Ok(sync),
        Err(err) => {
            flog_warn!(
                "output encode disabled for {:?} after encode failure: {err}",
                output_id
            );
            Ok(sync)
        }
    }
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
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    client_to_scene: &GlesTexProgram,
    srgb_to_linear: &GlesTexProgram,
) -> Result<SyncPoint> {
    targets.encoded_scanout = false;
    targets.encoded_hdr = false;
    targets.scene_linear = true;
    prepared.frame_ctx.damage = vec![Rectangle::from_loc_and_size((0, 0), buffer_size)];

    targets.ensure_offscreen(renderer, buffer_size)?;
    targets.ensure_linear_offscreen(renderer, buffer_size)?;
    targets.ensure_overlay_offscreen(renderer, buffer_size)?;

    let bg = state.theme.active_theme().background.color;
    let clear_color = Color32F::new(bg[0], bg[1], bg[2], bg[3]);
    let transparent = Color32F::new(0.0, 0.0, 0.0, 0.0);
    let empty_clients: [FlowRenderElement; 0] = [];
    let empty_popups: [FlowRenderElement; 0] = [];

    // Keep the shell base on its established encoded-sRGB rendering path.  In
    // particular, the wallpaper texture and several chrome pixel shaders are
    // authored for an 8-bit target.  Drawing that base directly into RGBA16F
    // produced corrupt, black regions on real DRM/NVIDIA outputs.  Decode the
    // completed opaque base into the linear target instead; color-managed
    // clients still retain their extended gamut in the FP16 passes below.
    {
        let sdr = targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("SDR base missing before linear scene draw"))?;
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
            &empty_clients,
            &empty_popups,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Base,
            ClientCompositingMode::Sdr,
            // The shell base is intentionally completed in the known-good SDR
            // target before the full-frame decode.  Keep the work-area glass in
            // that pass too: drawing its pixel shader separately into the FP16
            // target produces an incomplete diagonal quad on the direct NVIDIA
            // path.  Clients are still rendered later in scene-linear space and
            // therefore remain above the glass.
            ChromeGlassPass::InBaseSdr,
            false,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
    }

    {
        let sdr_base = targets.offscreen.as_ref().unwrap().texture.clone();
        let linear = targets
            .linear_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("linear scene missing before base decode"))?;
        let mut target = renderer
            .bind(&mut linear.texture)
            .map_err(|e| anyhow!("bind linear target for base decode: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin linear base decode frame: {e}"))?;
        clear_offscreen(&mut frame, buffer_size, transparent)
            .map_err(|e| anyhow!("clear linear base decode target: {e}"))?;
        let src_size = sdr_base.size();
        let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (src_size.w as f64, src_size.h as f64),
        );
        let dst_rect = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), buffer_size);
        frame
            .render_texture_from_to(
                &sdr_base,
                src_rect,
                dst_rect,
                std::slice::from_ref(&dst_rect),
                &[],
                Transform::Normal,
                1.0,
                Some(srgb_to_linear),
                &[],
            )
            .map_err(|e| anyhow!("decode SDR base into linear scene: {e}"))?;
        let _sync = frame
            .finish()
            .map_err(|e| anyhow!("finish linear base decode frame: {e}"))?;
    }

    let (client_elements, popup_elements) = {
        let linear = targets
            .linear_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("linear offscreen missing after allocation"))?;
        let mut target = renderer
            .bind(&mut linear.texture)
            .map_err(|e| anyhow!("bind linear SDR target: {e}"))?;
        let client_elements = build_output_client_elements(state, renderer, output_id);
        let popup_elements = build_output_popup_elements(state, renderer, output_id);
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin linear SDR frame: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Clients,
            ClientCompositingMode::Linear {
                client_to_scene: client_to_scene.clone(),
                srgb_to_linear: srgb_to_linear.clone(),
            },
            ChromeGlassPass::Skip,
            true,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame.finish()?;
        (client_elements, popup_elements)
    };

    {
        let linear = targets
            .linear_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("linear scene missing before overlay"))?;
        let mut target = renderer
            .bind(&mut linear.texture)
            .map_err(|e| anyhow!("bind linear overlay target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin linear overlay frame: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::Overlay,
            ClientCompositingMode::Linear {
                client_to_scene: client_to_scene.clone(),
                srgb_to_linear: srgb_to_linear.clone(),
            },
            ChromeGlassPass::Skip,
            true,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame
            .finish()
            .map_err(|e| anyhow!("finish linear overlay frame: {e}"))?;
    }

    // egui_glow and the software cursor produce sRGB. Render them into a
    // transparent 8-bit layer, decode that layer, and blend it into FP16.
    {
        let overlay = targets
            .overlay_offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("sRGB overlay target missing"))?;
        let mut target = renderer
            .bind(&mut overlay.texture)
            .map_err(|e| anyhow!("bind sRGB overlay target: {e}"))?;
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin sRGB overlay frame: {e}"))?;
        clear_offscreen(&mut frame, buffer_size, transparent)
            .map_err(|e| anyhow!("clear sRGB overlay target: {e}"))?;
        draw_output_stage(
            state,
            &mut frame,
            prepared,
            &client_elements,
            &popup_elements,
            ui_state,
            scene,
            output_state,
            OutputRenderStage::EguiOverlay,
            ClientCompositingMode::Sdr,
            ChromeGlassPass::Skip,
            false,
        )
        .map_err(|err| anyhow!("{err}"))?;
        let _sync = frame
            .finish()
            .map_err(|e| anyhow!("finish sRGB overlay frame: {e}"))?;
    }

    let overlay_texture = targets.overlay_offscreen.as_ref().unwrap().texture.clone();
    composite_srgb_overlay_onto_linear(
        renderer,
        &overlay_texture,
        &mut targets.linear_offscreen.as_mut().unwrap().texture,
        buffer_size,
        srgb_to_linear,
    )?;

    apply_linear_output_encode(state, renderer, targets, output_id, buffer_size)
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
    let srgb_to_linear = state.render.chrome_shaders.srgb_to_linear.clone();
    let use_linear = use_linear_sdr_path(renderer, targets, buffer_size)
        && client_to_scene.is_some()
        && srgb_to_linear.is_some();

    if use_linear {
        if let Err(err) = targets.ensure_linear_offscreen(renderer, buffer_size) {
            flog(format!(
                "Linear SDR disabled for offscreen render after FP16 allocation failed: {err}"
            ));
        }
    }

    targets.ensure_offscreen(renderer, buffer_size)?;

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
            ui_state,
            scene,
            output_state,
            client_to_scene.as_ref().unwrap(),
            srgb_to_linear.as_ref().unwrap(),
        )
    } else {
        run_sdr_pass(
            state,
            renderer,
            targets,
            output_id,
            buffer_size,
            &prepared,
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
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
) -> Result<SyncPoint> {
    targets.encoded_scanout = false;
    targets.encoded_hdr = false;
    targets.scene_linear = false;
    targets.ensure_offscreen(renderer, buffer_size)?;

    let sync = {
        let sdr = targets
            .offscreen
            .as_mut()
            .ok_or_else(|| anyhow!("offscreen texture missing before draw"))?;
        let mut target = renderer
            .bind(&mut sdr.texture)
            .map_err(|e| anyhow!("bind offscreen for draw: {e}"))?;
        let client_elements = build_output_client_elements(state, renderer, output_id);
        let popup_elements = build_output_popup_elements(state, renderer, output_id);
        let mut frame = renderer
            .render(&mut target, buffer_size, Transform::Normal)
            .map_err(|e| anyhow!("begin offscreen frame: {e}"))?;
        draw_output(
            state,
            &mut frame,
            prepared,
            &client_elements,
            &popup_elements,
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

#[cfg(test)]
mod tests {
    use super::resolve_hdr_encode_state;

    #[test]
    fn pending_hdr_enable_encodes_first_frame_as_pq() {
        assert_eq!(
            resolve_hdr_encode_state(true, true, true, false, Some(true), false),
            (true, true)
        );
    }

    #[test]
    fn pending_hdr_disable_encodes_first_frame_as_sdr() {
        assert_eq!(
            resolve_hdr_encode_state(true, false, true, true, Some(false), false),
            (false, false)
        );
    }

    #[test]
    fn ten_bit_scanout_does_not_enable_pq_by_itself() {
        assert_eq!(
            resolve_hdr_encode_state(true, false, true, false, None, false),
            (false, false)
        );
    }
}
