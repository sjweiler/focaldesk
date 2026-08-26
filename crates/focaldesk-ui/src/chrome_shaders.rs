// crates/focaldesk-engine/src/core/chrome_shader.rs
use focaldesk_logging::flog;

use smithay::backend::renderer::gles::GlesTexProgram;
use smithay::backend::renderer::gles::{
    GlesError, GlesPixelProgram, GlesRenderer, UniformName, UniformType,
};

// Smithay compiles custom shaders as GLSL ES 1.00. Keep one shared source for
// both GLES 2 and GLES 3 contexts; GLES 3 implementations accept this profile.
// The tests below guard the syntax and the optional fragment-highp fallback.

pub struct ChromeShaders {
    pub beveled_panel: Option<GlesPixelProgram>,
    pub light_channel: Option<GlesPixelProgram>,
    pub glass: Option<GlesPixelProgram>,
    pub recessed_button: Option<GlesPixelProgram>,
    pub top_bar: Option<GlesPixelProgram>,
    pub tinted_icon: Option<GlesTexProgram>,
    pub amber_lightbar: Option<GlesPixelProgram>,
    pub font_text: Option<GlesTexProgram>,
    pub rounded_rect: Option<GlesPixelProgram>,
    pub wallpaper_tint: Option<GlesTexProgram>,
    /// Adds wide-gamut SDR accents and conservative HDR10 highlight lifts
    /// from the SDR wallpaper after the retained base is decoded into the
    /// FP16 scene.
    pub wallpaper_creative_grade: Option<GlesTexProgram>,
    pub srgb_to_linear: Option<GlesTexProgram>,
    pub client_to_scene_linear: Option<GlesTexProgram>,
    /// Full FP16 scene-linear Rec.709 → encoded output color space.
    pub output_encode_linear: Option<GlesTexProgram>,
    /// FP16 scene-linear Rec.709 → tone-mapped SDR portal stream.
    pub portal_capture_sdr: Option<GlesTexProgram>,
    /// Full-frame scene sRGB → monitor encode (C1b).
    pub output_encode_sdr: Option<GlesTexProgram>,
    /// Encoded output color space → monitor ICC LUT encode (C2c).
    pub output_encode_lut: Option<GlesTexProgram>,
    /// Scene sRGB → linear scRGB (HDR working space, C3).
    pub sdr_to_linear_scrgb: Option<GlesTexProgram>,
    /// Linear scRGB (nits) → PQ-encoded HDR (C3).
    pub linear_scrgb_to_pq: Option<GlesTexProgram>,
    pub pulse: Option<GlesPixelProgram>,
    pub accent: Option<GlesPixelProgram>,
    pub flow_field: Option<GlesPixelProgram>,
    pub screensaver: Option<GlesPixelProgram>,
    pub glass_control: Option<GlesTexProgram>,
    /// Display-P3-authored variants used only while drawing into the FP16
    /// scene-linear SDR target. The legacy programs above remain the fallback.
    pub beveled_panel_wide: Option<GlesPixelProgram>,
    pub light_channel_wide: Option<GlesPixelProgram>,
    pub glass_wide: Option<GlesPixelProgram>,
    pub recessed_button_wide: Option<GlesPixelProgram>,
    pub top_bar_wide: Option<GlesPixelProgram>,
    pub tinted_icon_wide: Option<GlesTexProgram>,
    pub amber_lightbar_wide: Option<GlesPixelProgram>,
    pub font_text_wide: Option<GlesTexProgram>,
    pub rounded_rect_wide: Option<GlesPixelProgram>,
    pub wallpaper_tint_wide: Option<GlesTexProgram>,
    pub pulse_wide: Option<GlesPixelProgram>,
    pub accent_wide: Option<GlesPixelProgram>,
    pub flow_field_wide: Option<GlesPixelProgram>,
    pub screensaver_wide: Option<GlesPixelProgram>,
    pub glass_control_wide: Option<GlesTexProgram>,
    glass_control_disabled: bool,
    wide_gamut_disabled: bool,
}

impl Default for ChromeShaders {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
enum WideAlphaContract {
    Straight,
    Premultiplied,
}

/// Build a separate scene-linear wide-gamut program without changing the
/// established shader. The original main writes colors authored in linear
/// Display P3; this epilogue converts them to the compositor's extended
/// linear-Rec.709 working space. Values outside 0..1 are deliberately retained
/// for the final per-output color transform.
fn wide_gamut_variant(source: &str, alpha_contract: WideAlphaContract) -> String {
    // Capture the legacy result in an ordinary variable. Some GLES compilers
    // reject reading back `gl_FragColor`, even though they accept multiple
    // writes to it in the original program.
    let captured = source.replace("gl_FragColor", "fd_legacy_color");
    let renamed = captured.replacen(
        "void main()",
        "vec4 fd_legacy_color;\nvoid fd_legacy_main()",
        1,
    );
    let straight = match alpha_contract {
        WideAlphaContract::Straight => "fd_color.rgb",
        WideAlphaContract::Premultiplied => {
            "fd_color.a > 0.0001 ? fd_color.rgb / fd_color.a : fd_color.rgb"
        }
    };
    format!(
        r#"{renamed}

vec3 fd_display_p3_to_scene_linear(vec3 p3) {{
    return vec3(
        1.224745 * p3.r - 0.224904 * p3.g,
       -0.042058 * p3.r + 1.042081 * p3.g,
       -0.019642 * p3.r - 0.078655 * p3.g + 1.098537 * p3.b
    );
}}

void main() {{
    fd_legacy_main();
    vec4 fd_color = fd_legacy_color;
    vec3 fd_straight = {straight};
    vec3 fd_scene = fd_display_p3_to_scene_linear(fd_straight);
    gl_FragColor = vec4(fd_scene * fd_color.a, fd_color.a);
}}
"#
    )
}

fn wide_gamut_wallpaper_variant() -> String {
    // The sampled wallpaper starts as linear Rec.709 after its sRGB decode.
    // Move it into the same Display-P3 authoring coordinates as the tint before
    // the shared wide epilogue converts the mixed result back to scene space.
    let source = WALLPAPER_TINT_FRAG.replace(
        "src_rgb = srgb_to_linear(src.rgb);",
        r#"vec3 scene = srgb_to_linear(src.rgb);
        src_rgb = vec3(
            0.822593 * scene.r + 0.177534 * scene.g,
            0.033200 * scene.r + 0.966784 * scene.g,
            0.017085 * scene.r + 0.072396 * scene.g + 0.910301 * scene.b
        );"#,
    );
    wide_gamut_variant(&source, WideAlphaContract::Premultiplied)
}

fn wide_gamut_glass_control_variant() -> String {
    // The captured button background is already in scene-linear Rec.709.
    let source = GLASS_CONTROL_FRAG.replace(
        "vec3 background = texture2D(u_background, background_uv).rgb;",
        r#"vec3 scene_background = texture2D(u_background, background_uv).rgb;
    vec3 background = vec3(
        0.822593 * scene_background.r + 0.177534 * scene_background.g,
        0.033200 * scene_background.r + 0.966784 * scene_background.g,
        0.017085 * scene_background.r + 0.072396 * scene_background.g
            + 0.910301 * scene_background.b
    );"#,
    );
    wide_gamut_variant(&source, WideAlphaContract::Premultiplied)
}

impl ChromeShaders {
    pub fn new() -> Self {
        Self {
            beveled_panel: None,
            light_channel: None,
            glass: None,
            recessed_button: None,
            top_bar: None,
            tinted_icon: None,
            amber_lightbar: None,
            font_text: None,
            rounded_rect: None,
            wallpaper_tint: None,
            wallpaper_creative_grade: None,
            srgb_to_linear: None,
            client_to_scene_linear: None,
            output_encode_linear: None,
            portal_capture_sdr: None,
            output_encode_sdr: None,
            output_encode_lut: None,
            sdr_to_linear_scrgb: None,
            linear_scrgb_to_pq: None,
            pulse: None,
            accent: None,
            flow_field: None,
            screensaver: None,
            glass_control: None,
            beveled_panel_wide: None,
            light_channel_wide: None,
            glass_wide: None,
            recessed_button_wide: None,
            top_bar_wide: None,
            tinted_icon_wide: None,
            amber_lightbar_wide: None,
            font_text_wide: None,
            rounded_rect_wide: None,
            wallpaper_tint_wide: None,
            pulse_wide: None,
            accent_wide: None,
            flow_field_wide: None,
            screensaver_wide: None,
            glass_control_wide: None,
            glass_control_disabled: false,
            wide_gamut_disabled: false,
        }
    }

    /// True only when the complete alternate family compiled successfully.
    /// Callers use this as an all-or-nothing switch so one frame never mixes
    /// legacy encoded colors with Display-P3 scene-linear colors.
    pub fn wide_gamut_ready(&self) -> bool {
        !self.wide_gamut_disabled
            && self.beveled_panel_wide.is_some()
            && self.light_channel_wide.is_some()
            && self.glass_wide.is_some()
            && self.recessed_button_wide.is_some()
            && self.top_bar_wide.is_some()
            && self.tinted_icon_wide.is_some()
            && self.amber_lightbar_wide.is_some()
            && self.font_text_wide.is_some()
            && self.rounded_rect_wide.is_some()
            && self.wallpaper_tint_wide.is_some()
            && self.pulse_wide.is_some()
            && self.accent_wide.is_some()
            && self.flow_field_wide.is_some()
            && self.screensaver_wide.is_some()
            && self.glass_control_wide.is_some()
    }

    /// Compile only the programs needed by the standalone layer-shell chrome.
    /// The desktop renderer has many optional programs (HDR, portal, egui,
    /// glass controls); a shell client must not fail because one of those is
    /// unsupported by the client EGL context.
    pub fn ensure_shell_compiled(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        if self.beveled_panel.is_none() {
            match renderer.compile_custom_pixel_shader(
                BEVELED_PANEL_FRAG_V2,
                &[
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_glow_width", UniformType::_1f),
                    UniformName::new("u_glow_alpha", UniformType::_1f),
                    UniformName::new("u_inner_shadow", UniformType::_1f),
                    UniformName::new("u_corner_radius", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_light_color", UniformType::_4f),
                    UniformName::new("u_shadow_color", UniformType::_4f),
                    UniformName::new("u_glow_color", UniformType::_4f),
                ],
            ) {
                Ok(program) => self.beveled_panel = Some(program),
                Err(error) => eprintln!("focal shell beveled_panel shader: {error}"),
            }
        }
        if self.recessed_button.is_none() {
            match renderer.compile_custom_pixel_shader(
                RECESSED_BUTTON_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_inner_shadow", UniformType::_1f),
                    UniformName::new("u_glow_strength", UniformType::_1f),
                    UniformName::new("u_glow_radius", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_shadow_color", UniformType::_4f),
                    UniformName::new("u_glow_color", UniformType::_4f),
                ],
            ) {
                Ok(program) => self.recessed_button = Some(program),
                Err(error) => eprintln!("focal shell recessed_button shader: {error}"),
            }
        }
        if self.top_bar.is_none() {
            match renderer.compile_custom_pixel_shader(
                TOP_BAR_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_radius", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_highlight_strength", UniformType::_1f),
                    UniformName::new("u_shadow_strength", UniformType::_1f),
                    UniformName::new("u_trim_height", UniformType::_1f),
                    UniformName::new("u_trim_brightness", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_edge_color", UniformType::_4f),
                    UniformName::new("u_trim_color", UniformType::_4f),
                ],
            ) {
                Ok(program) => self.top_bar = Some(program),
                Err(error) => eprintln!("focal shell top_bar shader: {error}"),
            }
        }
        if self.glass_control.is_none() {
            match renderer.compile_custom_texture_shader(
                GLASS_CONTROL_FRAG,
                &[
                    UniformName::new("u_background", UniformType::_1i),
                    UniformName::new("u_background_uv_size", UniformType::_2f),
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_icon_uv_origin", UniformType::_2f),
                    UniformName::new("u_icon_uv_size", UniformType::_2f),
                    UniformName::new("u_icon_rect", UniformType::_4f),
                    UniformName::new("u_icon_texel_size", UniformType::_2f),
                    UniformName::new("u_glass_tint", UniformType::_4f),
                    UniformName::new("u_accent_color", UniformType::_3f),
                    UniformName::new("u_corner_radius", UniformType::_1f),
                    UniformName::new("u_border_width", UniformType::_1f),
                    UniformName::new("u_hover", UniformType::_1f),
                    UniformName::new("u_pressed", UniformType::_1f),
                    UniformName::new("u_enabled", UniformType::_1f),
                    UniformName::new("u_active", UniformType::_1f),
                    UniformName::new("u_warning", UniformType::_1f),
                    UniformName::new("u_light_dir", UniformType::_3f),
                    UniformName::new("u_opacity", UniformType::_1f),
                    UniformName::new("u_output_factor", UniformType::_1f),
                    UniformName::new("u_icon_strength", UniformType::_1f),
                    UniformName::new("u_etch_depth", UniformType::_1f),
                ],
            ) {
                Ok(program) => self.glass_control = Some(program),
                Err(error) => eprintln!("focal shell glass_control shader: {error}"),
            }
        }
        Ok(())
    }

    pub fn ensure_compiled(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        if self.beveled_panel.is_none() {
            flog("compiling beveled_panel shader");
            match renderer.compile_custom_pixel_shader(
                BEVELED_PANEL_FRAG_V2,
                &[
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_glow_width", UniformType::_1f),
                    UniformName::new("u_glow_alpha", UniformType::_1f),
                    UniformName::new("u_inner_shadow", UniformType::_1f),
                    UniformName::new("u_corner_radius", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_light_color", UniformType::_4f),
                    UniformName::new("u_shadow_color", UniformType::_4f),
                    UniformName::new("u_glow_color", UniformType::_4f),
                ],
            ) {
                Ok(program) => {
                    flog("beveled_panel compiled OK");
                    self.beveled_panel = Some(program);
                }
                Err(e) => {
                    flog(format!("beveled_panel compile failed: {:?}", e));
                    flog("==== BEVELED_PANEL_FRAG V2 ====");
                    flog(BEVELED_PANEL_FRAG_V2);
                    return Err(e);
                }
            }
        }
        if self.light_channel.is_none() {
            self.light_channel = Some(renderer.compile_custom_pixel_shader(
                LIGHT_CHANNEL_FRAG,
                &[
                    UniformName::new("u_slot_inset", UniformType::_1f),
                    UniformName::new("u_core_inset", UniformType::_1f),
                    UniformName::new("u_glow_radius", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_housing_color", UniformType::_4f),
                    UniformName::new("u_glow_color", UniformType::_4f),
                    UniformName::new("u_core_color", UniformType::_4f),
                ],
            )?);
        }

        if self.glass.is_none() {
            self.glass = Some(renderer.compile_custom_pixel_shader(
                WORKAREA_GLASS_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_opacity", UniformType::_1f),
                    UniformName::new("u_output_factor", UniformType::_1f),
                    UniformName::new("u_edge_width", UniformType::_1f),
                    UniformName::new("u_edge_brightness", UniformType::_1f),
                    UniformName::new("u_highlight_strength", UniformType::_1f),
                    UniformName::new("u_tint", UniformType::_4f),
                    UniformName::new("u_edge_color", UniformType::_4f),
                    UniformName::new("u_time", UniformType::_1f),
                ],
            )?);
        }

        if self.recessed_button.is_none() {
            self.recessed_button = Some(renderer.compile_custom_pixel_shader(
                RECESSED_BUTTON_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_inner_shadow", UniformType::_1f),
                    UniformName::new("u_glow_strength", UniformType::_1f),
                    UniformName::new("u_glow_radius", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_shadow_color", UniformType::_4f),
                    UniformName::new("u_glow_color", UniformType::_4f),
                ],
            )?);
        }

        if self.tinted_icon.is_none() {
            self.tinted_icon = Some(renderer.compile_custom_texture_shader(
                TINTED_ICON_FRAG,
                &[UniformName::new("u_tint", UniformType::_4f)],
            )?);
        }

        if self.glass_control.is_none() && !self.glass_control_disabled {
            match renderer.compile_custom_texture_shader(
                GLASS_CONTROL_FRAG,
                &[
                    UniformName::new("u_background", UniformType::_1i),
                    UniformName::new("u_background_uv_size", UniformType::_2f),
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_icon_uv_origin", UniformType::_2f),
                    UniformName::new("u_icon_uv_size", UniformType::_2f),
                    UniformName::new("u_icon_rect", UniformType::_4f),
                    UniformName::new("u_icon_texel_size", UniformType::_2f),
                    UniformName::new("u_glass_tint", UniformType::_4f),
                    UniformName::new("u_accent_color", UniformType::_3f),
                    UniformName::new("u_corner_radius", UniformType::_1f),
                    UniformName::new("u_border_width", UniformType::_1f),
                    UniformName::new("u_hover", UniformType::_1f),
                    UniformName::new("u_pressed", UniformType::_1f),
                    UniformName::new("u_enabled", UniformType::_1f),
                    UniformName::new("u_active", UniformType::_1f),
                    UniformName::new("u_warning", UniformType::_1f),
                    UniformName::new("u_light_dir", UniformType::_3f),
                    UniformName::new("u_opacity", UniformType::_1f),
                    UniformName::new("u_output_factor", UniformType::_1f),
                    UniformName::new("u_icon_strength", UniformType::_1f),
                    UniformName::new("u_etch_depth", UniformType::_1f),
                ],
            ) {
                Ok(program) => self.glass_control = Some(program),
                Err(err) => {
                    self.glass_control_disabled = true;
                    focaldesk_logging::flog_warn!(
                        "glass_control shader compile failed; keeping legacy chrome controls: {:?}",
                        err
                    );
                }
            }
        }

        if self.font_text.is_none() {
            self.font_text = Some(renderer.compile_custom_texture_shader(
                FONT_TEXT_FRAG,
                &[UniformName::new("u_tint", UniformType::_4f)],
            )?);
        }

        if self.wallpaper_tint.is_none() {
            self.wallpaper_tint = Some(renderer.compile_custom_texture_shader(
                WALLPAPER_TINT_FRAG,
                &[
                    UniformName::new("u_tint", UniformType::_4f),
                    UniformName::new("u_decode_srgb", UniformType::_1f),
                    UniformName::new("u_texel_size", UniformType::_2f),
                    UniformName::new("u_blur_radius", UniformType::_1f),
                    UniformName::new("u_saturation", UniformType::_1f),
                ],
            )?);
        }

        if self.wallpaper_creative_grade.is_none() {
            self.wallpaper_creative_grade = Some(renderer.compile_custom_texture_shader(
                WALLPAPER_CREATIVE_GRADE_FRAG,
                &[
                    UniformName::new("u_tint", UniformType::_4f),
                    UniformName::new("u_texel_size", UniformType::_2f),
                    UniformName::new("u_grade_mode", UniformType::_1f),
                    UniformName::new("u_reference_white_nits", UniformType::_1f),
                    UniformName::new("u_peak_nits", UniformType::_1f),
                ],
            )?);
        }

        if self.srgb_to_linear.is_none() {
            self.srgb_to_linear =
                Some(renderer.compile_custom_texture_shader(SRGB_TO_LINEAR_FRAG, &[])?);
        }

        if self.client_to_scene_linear.is_none() {
            self.client_to_scene_linear = Some(renderer.compile_custom_texture_shader(
                CLIENT_TO_SCENE_LINEAR_FRAG,
                &[
                    UniformName::new("u_decode_tf", UniformType::_1f),
                    UniformName::new("u_reference_white_nits", UniformType::_1f),
                    UniformName::new("u_linear_to_scene_scale", UniformType::_1f),
                    UniformName::new("u_m0", UniformType::_3f),
                    UniformName::new("u_m1", UniformType::_3f),
                    UniformName::new("u_m2", UniformType::_3f),
                    UniformName::new("u_src_bits", UniformType::_1f),
                ],
            )?);
        }

        if self.output_encode_linear.is_none() {
            self.output_encode_linear = Some(renderer.compile_custom_texture_shader(
                COMPOSITE_LINEAR_LAYER_FRAG,
                &[
                    UniformName::new("u_encode_tf", UniformType::_1f),
                    UniformName::new("u_m0", UniformType::_3f),
                    UniformName::new("u_m1", UniformType::_3f),
                    UniformName::new("u_m2", UniformType::_3f),
                ],
            )?);
        }

        if self.portal_capture_sdr.is_none() {
            self.portal_capture_sdr = Some(renderer.compile_custom_texture_shader(
                PORTAL_CAPTURE_SDR_FRAG,
                &[
                    UniformName::new("u_source_peak", UniformType::_1f),
                    UniformName::new("u_decode_srgb", UniformType::_1f),
                    UniformName::new("u_compress_gamut", UniformType::_1f),
                    UniformName::new("u_bt709_transfer", UniformType::_1f),
                    UniformName::new("u_m0", UniformType::_3f),
                    UniformName::new("u_m1", UniformType::_3f),
                    UniformName::new("u_m2", UniformType::_3f),
                ],
            )?);
        }

        if self.output_encode_sdr.is_none() {
            self.output_encode_sdr = Some(renderer.compile_custom_texture_shader(
                OUTPUT_ENCODE_SDR_FRAG,
                &[
                    UniformName::new("u_encode_tf", UniformType::_1f),
                    UniformName::new("u_m0", UniformType::_3f),
                    UniformName::new("u_m1", UniformType::_3f),
                    UniformName::new("u_m2", UniformType::_3f),
                ],
            )?);
        }

        if self.output_encode_lut.is_none() {
            match renderer.compile_custom_texture_shader(
                OUTPUT_ENCODE_LUT_FRAG,
                &[
                    UniformName::new("u_lut_tex", UniformType::_1i),
                    UniformName::new("u_grid", UniformType::_1f),
                ],
            ) {
                Ok(program) => {
                    focaldesk_logging::flog_info!("output_encode_lut shader compiled OK");
                    self.output_encode_lut = Some(program);
                }
                Err(err) => {
                    focaldesk_logging::flog_warn!(
                        "startup: output_encode_lut shader compile failed; disabling ICC LUT output encode for this session and falling back to parametric encode: {:?}",
                        err
                    );
                }
            }
        }

        if self.sdr_to_linear_scrgb.is_none() {
            self.sdr_to_linear_scrgb = Some(renderer.compile_custom_texture_shader(
                SDR_TO_LINEAR_SCRGB_FRAG,
                &[UniformName::new("u_sdr_white_nits", UniformType::_1f)],
            )?);
        }

        if self.linear_scrgb_to_pq.is_none() {
            self.linear_scrgb_to_pq = Some(renderer.compile_custom_texture_shader(
                LINEAR_SCRGB_TO_PQ_FRAG,
                &[
                    UniformName::new("u_max_nits", UniformType::_1f),
                    UniformName::new("u_sdr_white_nits", UniformType::_1f),
                    UniformName::new("u_saturation", UniformType::_1f),
                    UniformName::new("u_midtone_gamma", UniformType::_1f),
                    UniformName::new("u_calibration_pattern", UniformType::_1f),
                    UniformName::new("u_m0", UniformType::_3f),
                    UniformName::new("u_m1", UniformType::_3f),
                    UniformName::new("u_m2", UniformType::_3f),
                    UniformName::new("u_p0", UniformType::_3f),
                    UniformName::new("u_p1", UniformType::_3f),
                    UniformName::new("u_p2", UniformType::_3f),
                    UniformName::new("u_panel_luma", UniformType::_3f),
                ],
            )?);
        }

        if self.amber_lightbar.is_none() {
            self.amber_lightbar = Some(renderer.compile_custom_pixel_shader(
                AMBER_LIGHTBAR_FRAG,
                &[UniformName::new("u_color", UniformType::_4f)],
            )?);
        }

        if self.rounded_rect.is_none() {
            self.rounded_rect = Some(renderer.compile_custom_pixel_shader(
                ROUNDED_RECT_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f), // rect size in pixels
                    UniformName::new("u_radius", UniformType::_1f), // corner radius
                    UniformName::new("u_color", UniformType::_4f), // fill color (rgba)
                ],
            )?);
        }

        if self.top_bar.is_none() {
            self.top_bar = Some(renderer.compile_custom_pixel_shader(
                TOP_BAR_FRAG,
                &[
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_radius", UniformType::_1f),
                    UniformName::new("u_softness", UniformType::_1f),
                    UniformName::new("u_bevel", UniformType::_1f),
                    UniformName::new("u_highlight_strength", UniformType::_1f),
                    UniformName::new("u_shadow_strength", UniformType::_1f),
                    UniformName::new("u_trim_height", UniformType::_1f),
                    UniformName::new("u_trim_brightness", UniformType::_1f),
                    UniformName::new("u_face_color", UniformType::_4f),
                    UniformName::new("u_edge_color", UniformType::_4f),
                    UniformName::new("u_trim_color", UniformType::_4f),
                ],
            )?);
        }

        if self.pulse.is_none() {
            self.pulse = Some(renderer.compile_custom_pixel_shader(
                PULSE_FRAG,
                &[
                    UniformName::new("u_click_pos", UniformType::_2f),
                    UniformName::new("u_time", UniformType::_1f),
                    UniformName::new("u_size", UniformType::_2f),
                    UniformName::new("u_color", UniformType::_4f),
                ],
            )?);
        }

        if self.accent.is_none() {
            match renderer.compile_custom_pixel_shader(
                ACCENT_FRAG,
                &[
                    UniformName::new("u_resolution", UniformType::_2f),
                    UniformName::new("u_rect", UniformType::_4f),
                    UniformName::new("u_accent", UniformType::_4f),
                    UniformName::new("u_time", UniformType::_1f),
                    UniformName::new("u_pulse", UniformType::_1f),
                    UniformName::new("u_active", UniformType::_1f),
                ],
            ) {
                Ok(program) => {
                    self.accent = Some(program);
                }
                Err(err) => {
                    flog(format!(
                        "accent shader compile failed; disabling active-output glow: {:?}",
                        err
                    ));
                }
            }
        }

        if self.flow_field.is_none() {
            self.flow_field = Some(renderer.compile_custom_pixel_shader(
                FLOW_FIELD_FRAG,
                &[
                    UniformName::new("u_resolution", UniformType::_2f),
                    UniformName::new("u_rect", UniformType::_4f),
                    UniformName::new("u_time", UniformType::_1f),
                    UniformName::new("u_mode", UniformType::_1f),
                    UniformName::new("u_energy", UniformType::_1f),
                    UniformName::new("u_color", UniformType::_4f),
                ],
            )?);
        }

        if self.screensaver.is_none() {
            self.screensaver = Some(renderer.compile_custom_pixel_shader(
                SCREENSAVER_FRAG,
                &[
                    UniformName::new("u_resolution", UniformType::_2f),
                    UniformName::new("u_time", UniformType::_1f),
                ],
            )?);
        }

        // Wide-gamut programs are deliberately optional. A driver/compiler
        // rejection must never take down the proven SDR shader path.
        if !self.wide_gamut_disabled && self.flow_field_wide.is_none() {
            let result = (|| -> Result<_, GlesError> {
                macro_rules! pixel {
                    ($source:expr, $alpha:expr, $uniforms:expr) => {{
                        let source = wide_gamut_variant($source, $alpha);
                        renderer.compile_custom_pixel_shader(&source, $uniforms)?
                    }};
                }
                macro_rules! texture {
                    ($source:expr, $alpha:expr, $uniforms:expr) => {{
                        let source = wide_gamut_variant($source, $alpha);
                        renderer.compile_custom_texture_shader(&source, $uniforms)?
                    }};
                }

                self.beveled_panel_wide = Some(pixel!(
                    BEVELED_PANEL_FRAG_V2,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_bevel", UniformType::_1f),
                        UniformName::new("u_softness", UniformType::_1f),
                        UniformName::new("u_glow_width", UniformType::_1f),
                        UniformName::new("u_glow_alpha", UniformType::_1f),
                        UniformName::new("u_inner_shadow", UniformType::_1f),
                        UniformName::new("u_corner_radius", UniformType::_1f),
                        UniformName::new("u_face_color", UniformType::_4f),
                        UniformName::new("u_light_color", UniformType::_4f),
                        UniformName::new("u_shadow_color", UniformType::_4f),
                        UniformName::new("u_glow_color", UniformType::_4f),
                    ]
                ));
                self.light_channel_wide = Some(pixel!(
                    LIGHT_CHANNEL_FRAG,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_slot_inset", UniformType::_1f),
                        UniformName::new("u_core_inset", UniformType::_1f),
                        UniformName::new("u_glow_radius", UniformType::_1f),
                        UniformName::new("u_softness", UniformType::_1f),
                        UniformName::new("u_housing_color", UniformType::_4f),
                        UniformName::new("u_glow_color", UniformType::_4f),
                        UniformName::new("u_core_color", UniformType::_4f),
                    ]
                ));
                self.glass_wide = Some(pixel!(
                    WORKAREA_GLASS_FRAG,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_size", UniformType::_2f),
                        UniformName::new("u_opacity", UniformType::_1f),
                        UniformName::new("u_output_factor", UniformType::_1f),
                        UniformName::new("u_edge_width", UniformType::_1f),
                        UniformName::new("u_edge_brightness", UniformType::_1f),
                        UniformName::new("u_highlight_strength", UniformType::_1f),
                        UniformName::new("u_tint", UniformType::_4f),
                        UniformName::new("u_edge_color", UniformType::_4f),
                        UniformName::new("u_time", UniformType::_1f),
                    ]
                ));
                self.recessed_button_wide = Some(pixel!(
                    RECESSED_BUTTON_FRAG,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_size", UniformType::_2f),
                        UniformName::new("u_bevel", UniformType::_1f),
                        UniformName::new("u_softness", UniformType::_1f),
                        UniformName::new("u_inner_shadow", UniformType::_1f),
                        UniformName::new("u_glow_strength", UniformType::_1f),
                        UniformName::new("u_glow_radius", UniformType::_1f),
                        UniformName::new("u_face_color", UniformType::_4f),
                        UniformName::new("u_shadow_color", UniformType::_4f),
                        UniformName::new("u_glow_color", UniformType::_4f),
                    ]
                ));
                self.top_bar_wide = Some(pixel!(
                    TOP_BAR_FRAG,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_size", UniformType::_2f),
                        UniformName::new("u_radius", UniformType::_1f),
                        UniformName::new("u_softness", UniformType::_1f),
                        UniformName::new("u_bevel", UniformType::_1f),
                        UniformName::new("u_highlight_strength", UniformType::_1f),
                        UniformName::new("u_shadow_strength", UniformType::_1f),
                        UniformName::new("u_trim_height", UniformType::_1f),
                        UniformName::new("u_trim_brightness", UniformType::_1f),
                        UniformName::new("u_face_color", UniformType::_4f),
                        UniformName::new("u_edge_color", UniformType::_4f),
                        UniformName::new("u_trim_color", UniformType::_4f),
                    ]
                ));
                self.tinted_icon_wide = Some(texture!(
                    TINTED_ICON_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[UniformName::new("u_tint", UniformType::_4f)]
                ));
                self.amber_lightbar_wide = Some(pixel!(
                    AMBER_LIGHTBAR_FRAG,
                    WideAlphaContract::Straight,
                    &[UniformName::new("u_color", UniformType::_4f)]
                ));
                self.font_text_wide = Some(texture!(
                    FONT_TEXT_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[UniformName::new("u_tint", UniformType::_4f)]
                ));
                self.rounded_rect_wide = Some(pixel!(
                    ROUNDED_RECT_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[
                        UniformName::new("u_size", UniformType::_2f),
                        UniformName::new("u_radius", UniformType::_1f),
                        UniformName::new("u_color", UniformType::_4f),
                    ]
                ));
                self.wallpaper_tint_wide = Some({
                    let source = wide_gamut_wallpaper_variant();
                    renderer.compile_custom_texture_shader(
                        &source,
                        &[
                            UniformName::new("u_tint", UniformType::_4f),
                            UniformName::new("u_decode_srgb", UniformType::_1f),
                            UniformName::new("u_texel_size", UniformType::_2f),
                            UniformName::new("u_blur_radius", UniformType::_1f),
                            UniformName::new("u_saturation", UniformType::_1f),
                        ],
                    )?
                });
                self.pulse_wide = Some(pixel!(
                    PULSE_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[
                        UniformName::new("u_click_pos", UniformType::_2f),
                        UniformName::new("u_time", UniformType::_1f),
                        UniformName::new("u_size", UniformType::_2f),
                        UniformName::new("u_color", UniformType::_4f),
                    ]
                ));
                self.accent_wide = Some(pixel!(
                    ACCENT_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[
                        UniformName::new("u_resolution", UniformType::_2f),
                        UniformName::new("u_rect", UniformType::_4f),
                        UniformName::new("u_accent", UniformType::_4f),
                        UniformName::new("u_time", UniformType::_1f),
                        UniformName::new("u_pulse", UniformType::_1f),
                        UniformName::new("u_active", UniformType::_1f),
                    ]
                ));
                self.flow_field_wide = Some(pixel!(
                    FLOW_FIELD_FRAG,
                    WideAlphaContract::Premultiplied,
                    &[
                        UniformName::new("u_resolution", UniformType::_2f),
                        UniformName::new("u_rect", UniformType::_4f),
                        UniformName::new("u_time", UniformType::_1f),
                        UniformName::new("u_mode", UniformType::_1f),
                        UniformName::new("u_energy", UniformType::_1f),
                        UniformName::new("u_color", UniformType::_4f),
                    ]
                ));
                self.screensaver_wide = Some(pixel!(
                    SCREENSAVER_FRAG,
                    WideAlphaContract::Straight,
                    &[
                        UniformName::new("u_resolution", UniformType::_2f),
                        UniformName::new("u_time", UniformType::_1f),
                    ]
                ));
                self.glass_control_wide = Some({
                    let source = wide_gamut_glass_control_variant();
                    renderer.compile_custom_texture_shader(
                        &source,
                        &[
                            UniformName::new("u_background", UniformType::_1i),
                            UniformName::new("u_background_uv_size", UniformType::_2f),
                            UniformName::new("u_size", UniformType::_2f),
                            UniformName::new("u_icon_uv_origin", UniformType::_2f),
                            UniformName::new("u_icon_uv_size", UniformType::_2f),
                            UniformName::new("u_icon_rect", UniformType::_4f),
                            UniformName::new("u_icon_texel_size", UniformType::_2f),
                            UniformName::new("u_glass_tint", UniformType::_4f),
                            UniformName::new("u_accent_color", UniformType::_3f),
                            UniformName::new("u_corner_radius", UniformType::_1f),
                            UniformName::new("u_border_width", UniformType::_1f),
                            UniformName::new("u_hover", UniformType::_1f),
                            UniformName::new("u_pressed", UniformType::_1f),
                            UniformName::new("u_enabled", UniformType::_1f),
                            UniformName::new("u_active", UniformType::_1f),
                            UniformName::new("u_warning", UniformType::_1f),
                            UniformName::new("u_light_dir", UniformType::_3f),
                            UniformName::new("u_opacity", UniformType::_1f),
                            UniformName::new("u_output_factor", UniformType::_1f),
                            UniformName::new("u_icon_strength", UniformType::_1f),
                            UniformName::new("u_etch_depth", UniformType::_1f),
                        ],
                    )?
                });
                Ok(())
            })();
            if let Err(error) = result {
                self.wide_gamut_disabled = true;
                focaldesk_logging::flog_warn!(
                    "wide-gamut chrome shader compile failed; retaining legacy shaders: {:?}",
                    error
                );
            }
        }

        Ok(())
    }
}

// No #version line here.
// Smithay provides v_coords, size, alpha.
const BEVELED_PANEL_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_radius;
uniform float u_bevel;
uniform float u_softness;
uniform vec2  u_light_dir;
uniform vec4  u_face_color;
uniform vec4  u_light_color;
uniform vec4  u_shadow_color;

float sdRoundBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 uv = v_coords;
    vec2 p = uv * size - 0.5 * size;
    vec2 half_size = 0.5 * size - vec2(1.0);

    float d = sdRoundBox(p, half_size, u_radius);
    float mask = 1.0 - smoothstep(0.0, max(u_softness, 0.0001), d);

    vec2 n = normalize(vec2(0.7, -0.7));
    float lit = clamp(dot(normalize(vec2(-u_light_dir.x, u_light_dir.y)), -n), -1.0, 1.0);

    vec4 base = u_face_color;
    vec4 hi   = u_light_color * max(lit, 0.0);
    vec4 lo   = u_shadow_color * max(-lit, 0.0);

    vec4 color = base + hi - lo;
    color.a *= mask * alpha;

    gl_FragColor = color;
}
"#;

const LIGHT_CHANNEL_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_slot_inset;
uniform float u_core_inset;
uniform float u_glow_radius;
uniform float u_softness;

uniform vec4 u_housing_color;
uniform vec4 u_glow_color;
uniform vec4 u_core_color;

float sdRoundBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

float aa(float d, float s) {
    return 1.0 - smoothstep(0.0, max(s, 0.0001), d);
}

void main() {
    vec2 p = v_coords * size - 0.5 * size;
    vec2 half_size = 0.5 * size;

    float housing_d = sdRoundBox(p, half_size - vec2(1.0), 8.0);
    float slot_d    = sdRoundBox(p, half_size - vec2(u_slot_inset), 6.0);
    float core_d    = sdRoundBox(p, half_size - vec2(u_core_inset), 4.0);

    float housing = aa(housing_d, u_softness);
    float slot    = aa(slot_d, u_softness);
    float core    = aa(core_d, u_softness);

    float glow = 1.0 - smoothstep(0.0, u_glow_radius, max(core_d, 0.0));

    vec4 color = vec4(0.0);
    color += u_housing_color * housing;
    color += u_glow_color * glow * 0.75;
    color = mix(color, u_core_color, core);

    color.a *= alpha;
    gl_FragColor = color;
}
"#;

const CHAMFER_PANEL_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_chamfer;
uniform float u_bevel;
uniform float u_softness;
uniform vec2  u_light_dir;
uniform vec4  u_face_color;
uniform vec4  u_light_color;
uniform vec4  u_shadow_color;

float sdChamferBox(vec2 p, vec2 b, float c) {
    vec2 q = abs(p);

    float body = max(q.x - (b.x - c), q.y - (b.y - c));
    float diag = (q.x + q.y) - (b.x + b.y - c);

    return max(body, diag);
}

float aa(float d, float s) {
    return 1.0 - smoothstep(0.0, max(s, 0.0001), d);
}

void main() {
    vec2 p = v_coords * size - 0.5 * size;
    vec2 half_size = 0.5 * size - vec2(1.0);

    // distances
    float outer_d = sdChamferBox(p, half_size, u_chamfer);
    float inner_d = sdChamferBox(p, half_size - vec2(u_bevel), max(u_chamfer - u_bevel, 0.0));

    float outer_mask = aa(outer_d, u_softness);
    float inner_mask = aa(inner_d, u_softness);

    // sharpened bevel band
    float bevel_band =
        smoothstep(0.2, 0.0, outer_d) *
        (1.0 - smoothstep(0.0, -u_bevel, inner_d));

    float face_mask = inner_mask;

    // hard edge ridge (machined look)
    float edge = smoothstep(1.0, 0.0, abs(outer_d));

    // directional lighting (hard split, not gradient)
    vec2 l = normalize(u_light_dir);
    float side = (-p.x * l.x - p.y * l.y) / max(size.x, size.y);
    float light_side = step(0.0, side);

    // base color (slightly darkened so bevel pops)
    vec4 color = vec4(0.0);
    color += u_face_color * face_mask * 0.95;

    // bevel lighting (hard contrast)
    color += u_light_color * bevel_band * light_side * 1.2;
    color += u_shadow_color * bevel_band * (1.0 - light_side) * 1.4;

    // edge ridge highlight
    color += vec4(0.08, 0.12, 0.18, 1.0) * edge;

    // inner shadow (recess feel)
    float inner_shadow = smoothstep(-u_bevel, -u_bevel * 2.0, inner_d);
    color *= 1.0 - inner_shadow * 0.15;

    color.a = outer_mask * alpha;
    gl_FragColor = color;
}
"#;

const CHAMFER_OLD__PANEL_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_chamfer = 0.0;
uniform float u_bevel;
uniform float u_softness;
uniform vec2  u_light_dir;
uniform vec4  u_face_color;
uniform vec4  u_light_color;
uniform vec4  u_shadow_color;

float sdChamferBox(vec2 p, vec2 b, float c) {
    vec2 q = abs(p);

    float body = max(q.x - (b.x - c), q.y - (b.y - c));
    float diag = (q.x + q.y) - (b.x + b.y - c);

    return max(body, diag);
}

float aa(float d, float s) {
    return 1.0 - smoothstep(0.0, max(s, 0.0001), d);
}

void main() {
    vec2 p = v_coords * size - 0.5 * size;
    vec2 half_size = 0.5 * size - vec2(1.0);

    float outer_d = sdChamferBox(p, half_size, u_chamfer);
    float inner_d = sdChamferBox(p, half_size - vec2(u_bevel), max(u_chamfer - u_bevel, 0.0));

    float outer_mask = aa(outer_d, u_softness);
    float inner_mask = aa(inner_d, u_softness);

    float bevel_band = clamp(outer_mask - inner_mask, 0.0, 1.0);
    float face_mask   = inner_mask;

    vec2 l = normalize(u_light_dir);
    vec2 gp = abs(p);

    // cheap directional bevel cue
    float side =
        smoothstep(0.0, 1.0, (-p.x * l.x - p.y * l.y) / max(size.x, size.y));

    vec4 color = vec4(0.0);
    color += u_face_color * face_mask;
    color += u_light_color * bevel_band * side;
    color += u_shadow_color * bevel_band * (1.0 - side);

    color.a = outer_mask * alpha;
    gl_FragColor = color;
}
"#;

const BEVELED_PANEL_FRAG_V2: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_bevel;
uniform float u_softness;
uniform float u_glow_width;
uniform float u_glow_alpha;
uniform float u_inner_shadow;
uniform float u_corner_radius;

uniform vec4 u_face_color;
uniform vec4 u_light_color;
uniform vec4 u_shadow_color;
uniform vec4 u_glow_color;

float hash(float n) {
    return fract(sin(n) * 43758.5453123);
}

float rounded_box_sdf(vec2 p, vec2 box_size, float radius) {
    vec2 half_size = box_size * 0.5;
    vec2 q = abs(p - half_size) - (half_size - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    vec2 p = v_coords * size;

    float dl = p.x;
    float dt = p.y;
    float dr = size.x - p.x;
    float db = size.y - p.y;

    float bevel = max(u_bevel, 0.0001);
    float soft  = max(u_softness, 0.0001);
    float radius = clamp(u_corner_radius, 0.0, min(size.x, size.y) * 0.5);
    float outer_sdf = rounded_box_sdf(p, size, radius);
    float outer_mask = 1.0 - smoothstep(0.0, soft, outer_sdf);
    float edge_dist = max(-outer_sdf, 0.0);

    vec3 color = u_face_color.rgb;

    float top_w    = 1.0 - smoothstep(0.0, bevel + soft, dt);
    float left_w   = 1.0 - smoothstep(0.0, bevel + soft, dl);
    float bottom_w = 1.0 - smoothstep(0.0, bevel + soft, db);
    float right_w  = 1.0 - smoothstep(0.0, bevel + soft, dr);

    float light_amt  = max(top_w, left_w) * 0.85;
    float shadow_amt = max(bottom_w, right_w) * 0.90;

    // subtle face gradient: brighter upper-left, darker lower-right
    float face_grad = (1.0 - v_coords.y) * 0.65 + (1.0 - v_coords.x) * 0.35;
    face_grad = mix(0.94, 1.05, face_grad);
    color *= face_grad;

    color = mix(color, u_light_color.rgb, light_amt * 0.50);
    color = mix(color, u_shadow_color.rgb, shadow_amt * 0.58);

    float inner_shadow_px = max(u_inner_shadow, 0.0);
    if (inner_shadow_px > 0.0) {
        float inner_ring = 1.0 - smoothstep(bevel, bevel + inner_shadow_px, edge_dist);
        color = mix(color, u_shadow_color.rgb, inner_ring * 0.14);
    }

    // very weak brushed metal variation
    float brush = hash(floor(p.y * 0.75)) - 0.5;
    color += brush * 0.018;

    float glow_w = max(u_glow_width, 0.0);
    if (glow_w > 0.0 && u_glow_alpha > 0.0) {
        float glow_ring = 1.0 - smoothstep(bevel, bevel + glow_w, edge_dist);
        color += u_glow_color.rgb * glow_ring * u_glow_alpha * 0.35;
    }

    gl_FragColor = vec4(color, u_face_color.a * alpha * outer_mask);
}
"#;

// gl_FragColor = vec4(color, u_face_color.a * alpha);

const WORKAREA_GLASS_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 size;
uniform float alpha;

uniform float u_time;
uniform float u_edge_width;
uniform float u_edge_brightness;
uniform float u_highlight_strength;
uniform vec4 u_tint;
uniform vec4 u_edge_color;

float edge_mask(vec2 p, vec2 size, float w) {
    float left   = smoothstep(0.0, w, p.x);
    float right  = smoothstep(0.0, w, size.x - p.x);
    float top    = smoothstep(0.0, w, p.y);
    float bottom = smoothstep(0.0, w, size.y - p.y);
    return min(min(left, right), min(top, bottom));
}

void main() {
    vec2 uv = v_coords;
    vec2 p = uv * size;

    float e = edge_mask(p, size, u_edge_width);
    float edge = 1.0 - e;

    float diag = uv.x * 0.75 + uv.y * 0.25;
    float highlight = smoothstep(0.20, 0.52, diag) - smoothstep(0.52, 0.88, diag);
    highlight *= u_highlight_strength * 0.18;

    float top_band = smoothstep(0.0, 0.08, uv.y) * (1.0 - smoothstep(0.08, 0.24, uv.y));
    top_band *= 0.05;

    float shimmer_coord = uv.x * 0.85 + uv.y * 0.35;
    float shimmer = 0.5 + 0.5 * sin(shimmer_coord * 10.0 - u_time * 0.10);
    shimmer = smoothstep(0.80, 0.98, shimmer) * 0.007;

    float body_grad = 1.0 - uv.y;
    body_grad = mix(0.985, 1.015, body_grad);

    float inner_edge = 1.0 - smoothstep(0.03, 0.18, e);

    vec3 color = vec3(0.0);
    color += u_tint.rgb * 0.06;
    color *= body_grad;
    color += u_edge_color.rgb * edge * u_edge_brightness * 0.35;
    color += vec3(highlight);
    color += vec3(top_band);
    color += vec3(shimmer);
    color -= vec3(inner_edge * 0.035);

    float bezel = 1.0 - smoothstep(
        0.0, 2.5,
        min(min(p.x, size.x - p.x), min(p.y, size.y - p.y))
    );
    color -= vec3(bezel * 0.025);

    float noise = fract(sin(dot(floor(p), vec2(12.9898, 78.233))) * 43758.5453);
    color += vec3((noise - 0.5) * 0.0025);

    color = pow(max(color, vec3(0.0)), vec3(0.97));

    float out_alpha = alpha * 0.35;
    out_alpha += edge * 0.025;
    out_alpha += highlight * 0.08;
    out_alpha += top_band * 0.12;
    out_alpha += shimmer * 0.08;
    out_alpha = clamp(out_alpha, 0.0, 0.10);

    gl_FragColor = vec4(color, out_alpha);
}
"#;
const RECESSED_BUTTON_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 u_size;
uniform float u_bevel;
uniform float u_softness;
uniform float u_inner_shadow;
uniform float u_glow_strength;
uniform float u_glow_radius;
uniform vec4 u_face_color;
uniform vec4 u_shadow_color;
uniform vec4 u_glow_color;

float rounded_box_sdf(vec2 p, vec2 size, float radius) {
    vec2 q = abs(p - size * 0.5) - (size * 0.5 - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    vec2 p = v_coords * u_size;

    float radius = u_bevel;
    float sdf = rounded_box_sdf(p, u_size, radius);

    float edge = smoothstep(u_softness, 0.0, abs(sdf));

    // Inner shadow for recessed look
    float inner = smoothstep(0.0, u_softness, -sdf);

    vec3 color = u_face_color.rgb;

    // Recess shadow
    color -= inner * u_inner_shadow * u_shadow_color.rgb;

    // Center glow / backlight
    vec2 centered = v_coords - vec2(0.5);
    float dist = length(centered);
    float glow = smoothstep(u_glow_radius, 0.0, dist);
    color += glow * u_glow_strength * u_glow_color.rgb;

    // Slight edge darkening
    color = mix(color, u_shadow_color.rgb, edge * 0.2);

    float alpha = u_face_color.a * smoothstep(u_softness, 0.0, sdf);

    gl_FragColor = vec4(color, alpha);
}
"#;

const TOP_BAR_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform vec2 u_size;
uniform float u_radius;
uniform float u_softness;
uniform float u_bevel;
uniform float u_highlight_strength;
uniform float u_shadow_strength;
uniform float u_trim_height;
uniform float u_trim_brightness;
uniform vec4 u_face_color;
uniform vec4 u_edge_color;
uniform vec4 u_trim_color;

float rounded_box_sdf(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * u_size;
    vec2 center = u_size * 0.5;
    vec2 local = p - center;

    float sdf = rounded_box_sdf(local, u_size * 0.5, u_radius);

    // main mask
    float alpha = 1.0 - smoothstep(0.0, u_softness, sdf);

    // outer edge / bevel response
    float edge_band = 1.0 - smoothstep(u_bevel, u_bevel + u_softness, abs(sdf));

    // top reflection band
    float top_band = (1.0 - smoothstep(0.0, 0.22, v_coords.y)) * u_highlight_strength;

    // soft lower inner shadow
    float bottom_shadow = smoothstep(0.72, 1.0, v_coords.y) * u_shadow_strength;

    // faint horizontal material sweep so it does not feel dead flat
    float horiz = 0.5 + 0.5 * cos((v_coords.x - 0.5) * 3.14159);
    float face_variation = 0.035 * horiz;

    // trim line near top
    float trim_mask = 1.0 - smoothstep(
        u_trim_height,
        u_trim_height + max(1.0 / max(u_size.y, 1.0), 0.001),
        v_coords.y
    );

    vec3 color = u_face_color.rgb;

    // face treatment
    color *= 1.0 + face_variation;
    color += top_band * vec3(1.0);
    color -= bottom_shadow * u_edge_color.rgb * 0.8;

    // edge darkening
    color = mix(color, u_edge_color.rgb, edge_band * 0.22);

    // top trim
    color = mix(color, u_trim_color.rgb, trim_mask * u_trim_brightness);

    gl_FragColor = vec4(color, u_face_color.a * alpha);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn shader_sources() -> [&'static str; 27] {
        [
            BEVELED_PANEL_FRAG,
            LIGHT_CHANNEL_FRAG,
            CHAMFER_PANEL_FRAG,
            CHAMFER_OLD__PANEL_FRAG,
            BEVELED_PANEL_FRAG_V2,
            WORKAREA_GLASS_FRAG,
            RECESSED_BUTTON_FRAG,
            TOP_BAR_FRAG,
            TINTED_ICON_FRAG,
            CLIENT_TO_SCENE_LINEAR_FRAG,
            SRGB_TO_LINEAR_FRAG,
            COMPOSITE_LINEAR_LAYER_FRAG,
            PORTAL_CAPTURE_SDR_FRAG,
            OUTPUT_ENCODE_SDR_FRAG,
            OUTPUT_ENCODE_LUT_FRAG,
            SDR_TO_LINEAR_SCRGB_FRAG,
            LINEAR_SCRGB_TO_PQ_FRAG,
            AMBER_LIGHTBAR_FRAG,
            FONT_TEXT_FRAG,
            ROUNDED_RECT_FRAG,
            WALLPAPER_TINT_FRAG,
            WALLPAPER_CREATIVE_GRADE_FRAG,
            PULSE_FRAG,
            ACCENT_FRAG,
            FLOW_FIELD_FRAG,
            SCREENSAVER_FRAG,
            GLASS_CONTROL_FRAG,
        ]
    }

    #[test]
    fn shaders_stay_in_the_shared_glsl_es_100_subset() {
        for shader in shader_sources() {
            assert!(!shader.contains("#version"));
            assert!(!shader.contains("layout("));
            assert!(!shader.contains("texture("));
            assert!(!shader.contains("out vec4"));
            assert!(shader.contains("varying vec2 v_coords;"));
            assert!(shader.contains("gl_FragColor"));

            if shader.contains("precision highp float;") {
                assert!(shader.contains("GL_FRAGMENT_PRECISION_HIGH"));
                assert!(shader.contains("precision mediump float;"));
            }
        }
    }

    #[test]
    fn pixel_shaders_use_smithays_vertex_varying() {
        for shader in [RECESSED_BUTTON_FRAG, TOP_BAR_FRAG] {
            assert!(shader.contains("varying vec2 v_coords;"));
            assert!(!shader.contains("v_uv"));
        }
    }

    #[test]
    fn glass_control_uses_smithays_texture_shader_contract() {
        assert!(GLASS_CONTROL_FRAG.contains("//_DEFINES"));
        assert!(GLASS_CONTROL_FRAG.contains("varying vec2 v_coords;"));
        assert!(GLASS_CONTROL_FRAG.contains("uniform sampler2D tex;"));
        assert!(GLASS_CONTROL_FRAG.contains("uniform float alpha;"));
        assert!(GLASS_CONTROL_FRAG.contains("uniform sampler2D u_background;"));
        assert!(!GLASS_CONTROL_FRAG.contains("#version 300"));
        assert!(!GLASS_CONTROL_FRAG.contains("v_uv"));
    }

    #[test]
    fn tinted_icon_outputs_premultiplied_alpha() {
        assert!(TINTED_ICON_FRAG.contains("uniform float alpha;"));
        assert!(TINTED_ICON_FRAG.contains("vec4(u_tint.rgb * coverage, coverage)"));
    }

    #[test]
    fn linear_output_encode_never_discards_scanout_pixels() {
        assert!(!COMPOSITE_LINEAR_LAYER_FRAG.contains("discard;"));
        assert!(COMPOSITE_LINEAR_LAYER_FRAG.contains("vec4(encoded, 1.0)"));
    }

    #[test]
    fn portal_capture_tone_maps_luminance_before_gamut_compression() {
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("tone_map_luminance(luminance"));
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("mapped / luminance"));
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("compress_to_rec709(linear)"));
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("linear_to_srgb"));
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("linear_to_bt709"));
        assert!(PORTAL_CAPTURE_SDR_FRAG.contains("u_compress_gamut"));
    }

    #[test]
    fn hdr_pq_encode_tone_maps_luminance_instead_of_per_channel_clip() {
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("tone_map_nits(y, 10000.0, u_max_nits"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("max(white, display_peak * 0.8)"));
        assert!(!LINEAR_SCRGB_TO_PQ_FRAG.contains("min(white, display_peak * 0.9)"));
        assert!(!LINEAR_SCRGB_TO_PQ_FRAG.contains("max(white, display_peak * 0.9)"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("mapped / y"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("min(nits, vec3(10000.0))"));
        assert!(!LINEAR_SCRGB_TO_PQ_FRAG.contains("u_max_nits / peak_channel"));
        assert!(!LINEAR_SCRGB_TO_PQ_FRAG.contains("min(nits, vec3(u_max_nits))"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("compress_to_panel_gamut"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("mul_panel_to_bt2020"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("pq_output_dither(gl_FragCoord.xy)"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("/ 1023.0"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("uniform float u_saturation"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("uniform float u_midtone_gamma"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("uniform float u_calibration_pattern"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("(panel - vec3(panel_y)) * u_saturation"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("pow(normalized, u_midtone_gamma)"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("calibration_pattern_nits(v_coords)"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("200.0 / 0.2627"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("200.0 / 0.6780"));
        assert!(LINEAR_SCRGB_TO_PQ_FRAG.contains("200.0 / 0.0593"));
    }

    #[test]
    fn wallpaper_grade_is_scene_linear_and_preserves_the_black_floor() {
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("graded - base"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("vec4((graded - base) * alpha, 0.0)"));
        assert!(
            WALLPAPER_CREATIVE_GRADE_FRAG
                .contains("usable_peak = min(u_peak_nits, u_reference_white_nits * 2.0)")
        );
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("target_level(u_reference_white_nits)"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("p3_luminance(p3)"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("0.228975, 0.691739, 0.079287"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("cyan_highlight * 0.72"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("neutral_highlight = 1.0 - accent"));
        assert!(WALLPAPER_CREATIVE_GRADE_FRAG.contains("neutral_highlight * isolated"));
        assert!(!WALLPAPER_CREATIVE_GRADE_FRAG.contains("mix(600.0, 1000.0"));
        assert!(!WALLPAPER_CREATIVE_GRADE_FRAG.contains("mix(80.0, 800.0"));
        assert!(!WALLPAPER_CREATIVE_GRADE_FRAG.contains("base + vec3"));
    }

    #[test]
    fn wide_gamut_variants_are_separate_scene_linear_programs() {
        let legacy = FLOW_FIELD_FRAG.to_owned();
        let wide = wide_gamut_variant(FLOW_FIELD_FRAG, WideAlphaContract::Premultiplied);

        assert_eq!(legacy, FLOW_FIELD_FRAG);
        assert!(wide.contains("void fd_legacy_main()"));
        assert!(wide.contains("vec4 fd_legacy_color;"));
        assert!(wide.contains("vec4 fd_color = fd_legacy_color;"));
        assert!(wide.contains("fd_display_p3_to_scene_linear"));
        assert!(wide.contains("fd_scene * fd_color.a"));
        assert!(!FLOW_FIELD_FRAG.contains("fd_display_p3_to_scene_linear"));
    }

    #[test]
    fn wide_gamut_conversion_preserves_extended_scene_values() {
        let wide = wide_gamut_variant(AMBER_LIGHTBAR_FRAG, WideAlphaContract::Straight);
        assert!(wide.contains("1.224745 * p3.r - 0.224904 * p3.g"));
        assert!(!wide.contains("clamp(fd_scene"));
    }

    #[test]
    fn client_decode_preserves_scrgb_gamut_and_applies_white_scale() {
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("return c * u_linear_to_scene_scale;"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("if (!extended_range)"));
        assert!(!CLIENT_TO_SCENE_LINEAR_FRAG.contains("straight = max(straight, vec3(0.0))"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("u_src_bits"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("dither_code_value"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("dither * step"));
    }

    #[test]
    fn client_decode_keeps_bt1886_distinct_from_piecewise_srgb() {
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("vec3 bt1886_to_linear(vec3 c)"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("return bt1886_to_linear(c);"));
        assert!(CLIENT_TO_SCENE_LINEAR_FRAG.contains("u_decode_tf >= 3.5 && u_decode_tf < 4.5"));
    }

    #[test]
    fn sampled_scene_inputs_are_moved_into_p3_before_wide_effects() {
        let wallpaper = wide_gamut_wallpaper_variant();
        let glass = wide_gamut_glass_control_variant();
        assert!(wallpaper.contains("vec3 scene = srgb_to_linear(src.rgb)"));
        assert!(glass.contains("vec3 scene_background = texture2D"));
        assert!(wallpaper.contains("0.822593 * scene.r"));
        assert!(glass.contains("0.822593 * scene_background.r"));
    }
}

const TINTED_ICON_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform sampler2D tex;
uniform vec4 u_tint;
uniform float alpha;

void main() {
    vec4 src = texture2D(tex, v_coords);

    if (src.a < 0.01) {
        discard;
    }

    float coverage = src.a * u_tint.a * alpha;
    gl_FragColor = vec4(u_tint.rgb * coverage, coverage);
}
"#;

const CLIENT_TO_SCENE_LINEAR_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : enable
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_decode_tf;
uniform float u_reference_white_nits;
uniform float u_linear_to_scene_scale;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;
uniform float u_src_bits;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

vec3 gamma22_to_linear(vec3 c) {
    return pow(max(c, vec3(0.0)), vec3(2.2));
}

vec3 bt1886_to_linear(vec3 c) {
    // BT.1886 with a zero display-black offset reduces to a 2.4 power curve.
    // Keep it distinct from sRGB's piecewise toe for tagged video surfaces.
    return pow(max(c, vec3(0.0)), vec3(2.4));
}

vec3 pq_to_scene_linear(vec3 c) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    vec3 p = pow(clamp(c, 0.0, 1.0), vec3(1.0 / m2));
    vec3 normalized_nits = pow(
        max(p - c1, vec3(0.0)) / max(c2 - c3 * p, vec3(0.000001)),
        vec3(1.0 / m1)
    );
    vec3 nits = normalized_nits * 10000.0;
    return nits / max(u_reference_white_nits, 1.0);
}

vec3 dither_code_value(vec3 c) {
    // 8-bit client surfaces have 256 steps regardless of their transfer tag.
    // Ordered dither in code value hides dark ramps promoted into HDR10; it
    // cannot recover detail that was absent from the client buffer.
    if (u_src_bits <= 1.0) {
        return c;
    }
    float step = 1.0 / (exp2(u_src_bits) - 1.0);
    float dither = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715)))) - 0.5;
    return c + vec3(dither * step);
}

vec3 decode_color(vec3 c) {
    c = dither_code_value(c);
    if (u_decode_tf < 0.5) {
        return srgb_to_linear(c);
    }
    if (u_decode_tf < 1.5) {
        return c * u_linear_to_scene_scale;
    }
    if (u_decode_tf < 2.5) {
        return gamma22_to_linear(c);
    }
    if (u_decode_tf < 3.5) {
        return pq_to_scene_linear(c);
    }
    if (u_decode_tf < 4.5) {
        return srgb_to_linear(c);
    }
    return bt1886_to_linear(c);
}

vec3 mul_mat3(vec3 v) {
    return vec3(dot(u_m0, v), dot(u_m1, v), dot(u_m2, v));
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    if (src.a < 0.0001) {
        discard;
    }
    vec3 straight = src.rgb / src.a;
    // ExtLinear (Windows-scRGB) and extended sRGB (Chromium SRGB_HDR) keep
    // negatives and values above 1.0 for wide-gamut / HDR headroom.
    bool extended_range =
        (u_decode_tf >= 0.5 && u_decode_tf < 1.5) ||
        (u_decode_tf >= 3.5 && u_decode_tf < 4.5);
    if (!extended_range) {
        straight = clamp(straight, 0.0, 1.0);
    }
    vec3 linear = mul_mat3(decode_color(straight));
    gl_FragColor = vec4(linear * src.a, src.a) * alpha;
}
"#;

const SRGB_TO_LINEAR_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 straight = src.a > 0.0 ? clamp(src.rgb / src.a, 0.0, 1.0) : vec3(0.0);
    gl_FragColor = vec4(srgb_to_linear(straight) * src.a, src.a) * alpha;
}
"#;

const COMPOSITE_LINEAR_LAYER_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_encode_tf;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;

vec3 linear_to_srgb(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.0031308));
    vec3 low = c * 12.92;
    vec3 high = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, vec3(cutoff));
}

vec3 linear_to_gamma22(vec3 c) {
    return pow(max(c, vec3(0.0)), vec3(1.0 / 2.2));
}

vec3 mul_mat3(vec3 v) {
    return vec3(dot(u_m0, v), dot(u_m1, v), dot(u_m2, v));
}

vec3 encode_color(vec3 c) {
    if (u_encode_tf < 0.5) {
        return linear_to_srgb(c);
    }
    if (u_encode_tf < 1.5) {
        return c;
    }
    return linear_to_gamma22(c);
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    // This program encodes the complete, opaque scene.  It used to composite
    // a transparent client-only layer and retained that path's `discard`.
    // Discarding here leaves undefined pixels in the scanout texture (seen as
    // the large black triangle on NVIDIA).  The base pass guarantees opaque
    // coverage, but keep the zero-alpha fallback deterministic as well.
    vec3 straight = src.a > 0.0001 ? src.rgb / src.a : src.rgb;
    vec3 encoded = encode_color(mul_mat3(straight));
    gl_FragColor = vec4(encoded, 1.0) * alpha;
}
"#;

/// Scene-linear Rec.709 → SDR sRGB/Rec.709 portal contract.
///
/// Values through the SDR knee are unchanged. HDR headroom is compressed by
/// luminance so highlights retain hue, then extended-gamut RGB is pulled toward
/// equal-luminance neutral before the final legal-range clamp.
const PORTAL_CAPTURE_SDR_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_source_peak;
uniform float u_decode_srgb;
uniform float u_compress_gamut;
uniform float u_bt709_transfer;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;

vec3 linear_to_srgb(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.0031308));
    vec3 low = c * 12.92;
    vec3 high = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, vec3(cutoff));
}

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow(max((c + 0.055) / 1.055, vec3(0.0)), vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

vec3 linear_to_bt709(vec3 c) {
    bvec3 cutoff = lessThan(c, vec3(0.018));
    vec3 low = c * 4.5;
    vec3 high = 1.099 * pow(max(c, vec3(0.0)), vec3(0.45)) - 0.099;
    return mix(high, low, vec3(cutoff));
}

vec3 mul_mat3(vec3 v) {
    return vec3(dot(u_m0, v), dot(u_m1, v), dot(u_m2, v));
}

float tone_map_luminance(float value, float source_peak) {
    const float knee = 0.75;
    if (source_peak <= 1.0 || value <= knee) {
        return value;
    }
    float peak = max(source_peak, knee + 0.0001);
    float denominator = 1.0 - exp(-(peak - knee) / (1.0 - knee));
    float numerator = 1.0 - exp(-(value - knee) / (1.0 - knee));
    return knee + (1.0 - knee) * numerator / max(denominator, 0.0001);
}

vec3 compress_to_rec709(vec3 rgb) {
    rgb = max(rgb, vec3(0.0));
    float maximum = max(rgb.r, max(rgb.g, rgb.b));
    if (maximum <= 1.0) {
        return rgb;
    }
    float luminance = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
    vec3 neutral = vec3(clamp(luminance, 0.0, 1.0));
    float denominator = maximum - neutral.r;
    float amount = denominator > 0.0001 ? (maximum - 1.0) / denominator : 1.0;
    return mix(rgb, neutral, clamp(amount, 0.0, 1.0));
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 scene = src.a > 0.0001 ? src.rgb / src.a : src.rgb;
    if (u_decode_srgb > 0.5) {
        scene = srgb_to_linear(scene);
    }
    vec3 linear = max(mul_mat3(scene), vec3(0.0));
    float luminance = dot(linear, vec3(0.2126, 0.7152, 0.0722));
    if (u_source_peak > 1.0 && luminance > 0.000001) {
        float mapped = tone_map_luminance(luminance, u_source_peak);
        linear *= mapped / luminance;
    }
    if (u_compress_gamut > 0.5) {
        linear = compress_to_rec709(linear);
    }
    linear = clamp(linear, 0.0, 1.0);
    vec3 encoded = u_bt709_transfer > 0.5
        ? linear_to_bt709(linear)
        : linear_to_srgb(linear);
    gl_FragColor = vec4(encoded, 1.0) * alpha;
}
"#;

/// Scene-linear Rec.709 sRGB framebuffer → monitor primaries + transfer.
const OUTPUT_ENCODE_SDR_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_encode_tf;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

vec3 linear_to_srgb(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.0031308));
    vec3 low = c * 12.92;
    vec3 high = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, vec3(cutoff));
}

vec3 linear_to_gamma22(vec3 c) {
    return pow(max(c, vec3(0.0)), vec3(1.0 / 2.2));
}

vec3 mul_mat3(vec3 v) {
    return vec3(dot(u_m0, v), dot(u_m1, v), dot(u_m2, v));
}

vec3 encode_color(vec3 c) {
    if (u_encode_tf < 0.5) {
        return linear_to_srgb(c);
    }
    if (u_encode_tf < 1.5) {
        return c;
    }
    return linear_to_gamma22(c);
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 straight = src.a > 0.0001 ? clamp(src.rgb / src.a, 0.0, 1.0) : src.rgb;
    vec3 encoded = encode_color(mul_mat3(srgb_to_linear(straight)));
    gl_FragColor = vec4(encoded * src.a, src.a) * alpha;
}
"#;

/// Scene sRGB framebuffer → monitor ICC LUT (2D atlas, trilinear).
/// Always `sampler2D tex` — this pass only reads offscreen FBOs, never EGL external textures.
/// Smithay may define EXTERNAL for custom texture shaders; using samplerExternalOES here breaks
/// on NVIDIA GLES (C7531) when combined with the second sampler2D LUT atlas.
const OUTPUT_ENCODE_LUT_FRAG: &str = r#"
//_DEFINES_

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

uniform sampler2D tex;

varying vec2 v_coords;
uniform float alpha;
uniform sampler2D u_lut_tex;
uniform float u_grid;

vec3 lut_sample_at(vec3 cell) {
    float n = u_grid;
    // Explicitly clamp cell bounds to prevent NVIDIA hardware filtering artifacts at edges
    vec3 clamped_cell = clamp(cell, 0.0, n - 1.0);
    vec2 uv = vec2((clamped_cell.y * n + clamped_cell.x + 0.5) / (n * n), (clamped_cell.z + 0.5) / n);
    return texture2D(u_lut_tex, uv).rgb;
}

vec3 lut_lookup(vec3 c) {
    float n = u_grid;
    vec3 x = clamp(c, 0.0, 1.0) * (n - 1.0);
    
    // Improved rounding safety for NVIDIA compiler optimization passes
    vec3 i0 = clamp(floor(x + 1e-5), 0.0, n - 1.0);
    vec3 f = clamp(x - i0, 0.0, 1.0);
    vec3 i1 = min(i0 + 1.0, vec3(n - 1.0));

    vec3 c000 = lut_sample_at(vec3(i0.x, i0.y, i0.z));
    vec3 c100 = lut_sample_at(vec3(i1.x, i0.y, i0.z));
    vec3 c010 = lut_sample_at(vec3(i0.x, i1.y, i0.z));
    vec3 c110 = lut_sample_at(vec3(i1.x, i1.y, i0.z));
    vec3 c001 = lut_sample_at(vec3(i0.x, i0.y, i1.z));
    vec3 c101 = lut_sample_at(vec3(i1.x, i0.y, i1.z));
    vec3 c011 = lut_sample_at(vec3(i0.x, i1.y, i1.z));
    vec3 c111 = lut_sample_at(vec3(i1.x, i1.y, i1.z));

    vec3 c00 = mix(c000, c100, f.x);
    vec3 c10 = mix(c010, c110, f.x);
    vec3 c01 = mix(c001, c101, f.x);
    vec3 c11 = mix(c011, c111, f.x);
    vec3 c0 = mix(c00, c10, f.y);
    vec3 c1 = mix(c01, c11, f.y);
    return mix(c0, c1, f.z);
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    // Guard against division-by-zero or NaN injection 
    vec3 straight = src.a > 0.0001 ? clamp(src.rgb / src.a, 0.0, 1.0) : src.rgb;
    vec3 encoded = lut_lookup(straight);
    gl_FragColor = vec4(encoded * src.a, src.a) * alpha;
}
"#;
/// Scene sRGB → linear scRGB extended (nits above reference white).
const SDR_TO_LINEAR_SCRGB_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_sdr_white_nits;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 straight = src.a > 0.0001 ? clamp(src.rgb / src.a, 0.0, 1.0) : src.rgb;
    vec3 linear = srgb_to_linear(straight);
    vec3 scrgb = linear * (u_sdr_white_nits / 80.0);
    gl_FragColor = vec4(scrgb * src.a, src.a) * alpha;
}
"#;

/// Linear scRGB (nits) → SMPTE ST 2084 PQ (0–1 electrical).
const LINEAR_SCRGB_TO_PQ_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform float u_max_nits;
uniform float u_sdr_white_nits;
uniform float u_saturation;
uniform float u_midtone_gamma;
uniform float u_calibration_pattern;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;
uniform vec3 u_p0;
uniform vec3 u_p1;
uniform vec3 u_p2;
uniform vec3 u_panel_luma;

vec3 mul_mat3(vec3 v) {
    return vec3(dot(u_m0, v), dot(u_m1, v), dot(u_m2, v));
}

vec3 mul_panel_to_bt2020(vec3 v) {
    return vec3(dot(u_p0, v), dot(u_p1, v), dot(u_p2, v));
}

// Pull Rec.2020-only colors into this output's ICC/EDID volume. In-gamut
// P3/sRGB stays put. A near-Rec.2020 panel never hits the negative branch.
vec3 compress_to_panel_gamut(vec3 rgb) {
    float minc = min(rgb.r, min(rgb.g, rgb.b));
    if (minc >= 0.0) {
        return rgb;
    }
    float y = max(dot(rgb, u_panel_luma), 0.0);
    vec3 dest = vec3(y);
    float t = 0.0;
    if (rgb.r < 0.0 && abs(rgb.r - dest.r) > 0.000001) {
        t = max(t, rgb.r / (rgb.r - dest.r));
    }
    if (rgb.g < 0.0 && abs(rgb.g - dest.g) > 0.000001) {
        t = max(t, rgb.g / (rgb.g - dest.g));
    }
    if (rgb.b < 0.0 && abs(rgb.b - dest.b) > 0.000001) {
        t = max(t, rgb.b / (rgb.b - dest.b));
    }
    return mix(rgb, dest, clamp(t, 0.0, 1.0));
}

float pq_oetf(float nits) {
    float L = max(nits, 0.0) / 10000.0;
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float Lm = pow(L, m1);
    return pow((c1 + c2 * Lm) / (1.0 + c3 * Lm), m2);
}

float pq_output_dither(vec2 pixel) {
    // Stable, zero-mean noise in [-0.5, 0.5) PQ code values. Apply it after
    // ST 2084 so the final 10-bit quantization does not expose contours from
    // the otherwise floating-point composition and tone-map passes.
    return fract(52.9829189 * fract(dot(floor(pixel), vec2(0.06711056, 0.00583715)))) - 0.5;
}

float bt2020_luminance(vec3 nits) {
    return dot(nits, vec3(0.2627, 0.6780, 0.0593));
}

vec3 calibration_pattern_nits(vec2 coords) {
    // Upper half: fixed 100/203/300-nit neutrals and the configured peak.
    // Lower half: equal-200-nit BT.2020 primaries plus a 0-to-peak ramp.
    // Primary channel values exceed their luminance by the reciprocal luma
    // coefficient; that is expected for a true equal-luminance stimulus.
    if (coords.y < 0.5) {
        float gray_nits;
        if (coords.x < 0.25) {
            gray_nits = 100.0;
        } else if (coords.x < 0.5) {
            gray_nits = 203.0;
        } else if (coords.x < 0.75) {
            gray_nits = 300.0;
        } else {
            gray_nits = u_max_nits;
        }
        return vec3(gray_nits);
    }
    if (coords.x < 0.25) {
        return vec3(200.0 / 0.2627, 0.0, 0.0);
    }
    if (coords.x < 0.5) {
        return vec3(0.0, 200.0 / 0.6780, 0.0);
    }
    if (coords.x < 0.75) {
        return vec3(0.0, 0.0, 200.0 / 0.0593);
    }
    float ramp = clamp((coords.x - 0.75) * 4.0, 0.0, 1.0);
    return vec3(ramp * u_max_nits);
}

// Keep graphics white and most in-range HDR. Roll from 80% of panel peak
// so 400 nit content is not crushed from paper white, while extremes still
// compress. A per-channel min() of the same values turns clouds magenta.
float tone_map_nits(float value, float source_peak, float display_peak, float white) {
    float knee = max(white, display_peak * 0.8);
    if (value <= knee || display_peak <= knee) {
        return min(value, display_peak);
    }
    float peak = max(source_peak, knee + 0.0001);
    float range = max(display_peak - knee, 0.0001);
    float denominator = 1.0 - exp(-(peak - knee) / range);
    float numerator = 1.0 - exp(-(value - knee) / range);
    return knee + range * numerator / max(denominator, 0.0001);
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 scene_linear = src.a > 0.0001 ? src.rgb / src.a : src.rgb;
    bool calibration = u_calibration_pattern > 0.5;
    vec3 nits;
    if (calibration) {
        // The pattern is already absolute linear BT.2020. Bypass the creative
        // controls so a measurement does not depend on the current preset.
        nits = calibration_pattern_nits(v_coords);
    } else {
        // Scene Rec.709 → panel volume → BT.2020 PQ. HDR10 on the wire stays
        // Rec.2020; out-of-gamut colors are mapped like Windows WDDM.
        vec3 panel = compress_to_panel_gamut(mul_mat3(scene_linear));
        float panel_y = max(dot(panel, u_panel_luma), 0.0);
        panel = compress_to_panel_gamut(
            vec3(panel_y) + (panel - vec3(panel_y)) * u_saturation
        );
        nits = max(mul_panel_to_bt2020(panel), vec3(0.0)) * u_sdr_white_nits;
    }
    float y = bt2020_luminance(nits);
    if (y > 0.0001) {
        // Shape only the SDR range, keeping black and reference white fixed.
        // Values above reference white retain their authored HDR luminance.
        if (!calibration && y < u_sdr_white_nits) {
            float normalized = clamp(y / u_sdr_white_nits, 0.0, 1.0);
            float shaped = pow(normalized, u_midtone_gamma) * u_sdr_white_nits;
            nits *= shaped / y;
            y = shaped;
        }
        float mapped = tone_map_nits(y, 10000.0, u_max_nits, u_sdr_white_nits);
        nits *= mapped / y;
    }
    // PQ legal range is 0–10,000 nits per channel. Do not also clamp each
    // channel to the panel luminance peak: Rec.2020 blue's Y weight is 0.0593,
    // so a 400 nit sky has ~6,700 nits in B and that scale would leave ~27 nits.
    nits = min(nits, vec3(10000.0));
    vec3 pq = vec3(pq_oetf(nits.r), pq_oetf(nits.g), pq_oetf(nits.b));
    pq = clamp(pq + vec3(pq_output_dither(gl_FragCoord.xy) / 1023.0), 0.0, 1.0);
    float output_alpha = calibration ? 1.0 : src.a;
    gl_FragColor = vec4(pq * output_alpha, output_alpha) * alpha;
}
"#;

/*
const TINTED_ICON_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform sampler2D tex;
uniform float alpha;
uniform vec4 u_tint;

void main() {
    vec4 src = texture2D(tex, v_coords);

    // White atlas → apply tint
    gl_FragColor = vec4(u_tint.rgb, src.a * u_tint.a * alpha);
}
"#;
*/

const AMBER_LIGHTBAR_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform float alpha;
uniform vec4 u_color;

void main() {
    // v_coords.y: 0.0 top → 1.0 bottom
    float y = v_coords.y;

    // bright core near the top
    float core = smoothstep(0.45, 0.05, y);

    // softer glow extending downward
    float glow = smoothstep(1.0, 0.0, y) * 0.45;

    float intensity = max(core, glow);

    // tiny hot edge at very top
    float hot = smoothstep(0.08, 0.0, y);

    vec3 color = u_color.rgb * (0.75 + hot * 0.45);

    gl_FragColor = vec4(color, u_color.a * intensity * alpha);
}
"#;

const FONT_TEXT_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform sampler2D tex;
uniform vec4 u_tint;

void main() {
    vec4 src = texture2D(tex, v_coords);

    // All channels repeat coverage; smithay blends with ONE, ONE_MINUS_SRC_ALPHA (premultiplied).
    // Use min(R,G,B) so a bogus saturated alpha channel cannot flatten the glyph to a solid quad.
    float cov = min(src.r, min(src.g, src.b));

    vec3 rgb = u_tint.rgb * (cov * u_tint.a);
    float alpha = cov * u_tint.a;

    gl_FragColor = vec4(rgb, alpha);
}
"#;

// make rectangle have rounded corners
const ROUNDED_RECT_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

// Smithay pixel shaders use `texture.vert`: varying is `v_coords` (0..1 over the dest quad).
varying vec2 v_coords;

uniform vec2 u_size;
uniform float u_radius;
uniform vec4 u_color;

// Same SDF as TOP_BAR: `p` is relative to rect center, `half_size` is (w/2, h/2).
float rounded_box_sdf(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * u_size - (u_size * 0.5);

    float r = min(u_radius, min(u_size.x, u_size.y) * 0.5);

    float sdf = rounded_box_sdf(p, u_size * 0.5, r);

    // Inside the shape sdf < 0; soften edge over ~1 px in SDF space.
    float aa = 1.0;
    float mask = 1.0 - smoothstep(-aa, aa, sdf);
    float a = u_color.a * mask;

    // Premultiplied for smithay's ONE / ONE_MINUS_SRC_ALPHA blending.
    gl_FragColor = vec4(u_color.rgb * a, a);
}
"#;

const WALLPAPER_TINT_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

uniform sampler2D tex;
uniform vec4 u_tint;
uniform float u_decode_srgb;
uniform vec2 u_texel_size;
uniform float u_blur_radius;
uniform float u_saturation;

varying vec2 v_coords;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

void main() {
    vec2 offset = u_texel_size * min(max(u_blur_radius, 0.0), 64.0);
    vec4 src = texture2D(tex, v_coords) * 0.20;
    src += texture2D(tex, v_coords + vec2(offset.x, 0.0)) * 0.12;
    src += texture2D(tex, v_coords - vec2(offset.x, 0.0)) * 0.12;
    src += texture2D(tex, v_coords + vec2(0.0, offset.y)) * 0.12;
    src += texture2D(tex, v_coords - vec2(0.0, offset.y)) * 0.12;
    src += texture2D(tex, v_coords + offset) * 0.08;
    src += texture2D(tex, v_coords - offset) * 0.08;
    src += texture2D(tex, v_coords + vec2(offset.x, -offset.y)) * 0.08;
    src += texture2D(tex, v_coords + vec2(-offset.x, offset.y)) * 0.08;

    vec3 src_rgb = src.rgb;
    if (u_decode_srgb > 0.5) {
        src_rgb = srgb_to_linear(src.rgb);
    }

    float luminance = dot(src_rgb, vec3(0.2126, 0.7152, 0.0722));
    src_rgb = mix(vec3(luminance), src_rgb, clamp(u_saturation, 0.0, 2.0));

    vec3 rgb = mix(src_rgb, u_tint.rgb, u_tint.a);

    gl_FragColor = vec4(rgb * src.a, src.a);
}
"#;

/// Display-aware creative re-grade for the bundled SDR wallpaper.
///
/// The retained wallpaper/chrome base is deliberately rendered through the
/// reliable 8-bit path first. This program is then drawn over only the
/// wallpaper rectangle in the FP16 scene. It outputs an additive linear-light
/// delta (alpha zero under Smithay's ONE/ONE_MINUS_SRC_ALPHA blend), preserving
/// the SDR base while allowing selected pixels to exceed reference white.
const WALLPAPER_CREATIVE_GRADE_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;
uniform float alpha;
uniform vec4 u_tint;
uniform vec2 u_texel_size;
uniform float u_grade_mode; // 1 = wide-gamut SDR, 2 = HDR
uniform float u_reference_white_nits;
uniform float u_peak_nits;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

float luminance(vec3 c) {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

float p3_luminance(vec3 c) {
    return dot(c, vec3(0.228975, 0.691739, 0.079287));
}

vec3 scene_to_p3(vec3 c) {
    return vec3(
        0.822593 * c.r + 0.177534 * c.g,
        0.033200 * c.r + 0.966784 * c.g,
        0.017085 * c.r + 0.072396 * c.g + 0.910301 * c.b
    );
}

vec3 p3_to_scene(vec3 c) {
    return vec3(
        1.224745 * c.r - 0.224904 * c.g,
       -0.042058 * c.r + 1.042081 * c.g,
       -0.019642 * c.r - 0.078655 * c.g + 1.098537 * c.b
    );
}

vec3 raise_to_luminance(vec3 color, float target) {
    float y = max(luminance(color), 0.0001);
    return color * max(1.0, target / y);
}

float target_level(float nits) {
    return min(nits, u_peak_nits) / max(u_reference_white_nits, 1.0);
}

void main() {
    if (u_grade_mode < 0.5) {
        discard;
    }

    vec3 source_srgb = texture2D(tex, v_coords).rgb;
    vec3 base_srgb = mix(source_srgb, u_tint.rgb, u_tint.a);
    vec3 base = srgb_to_linear(base_srgb);
    float y = luminance(base);

    // Expand only the authored cyan/orange accents into P3. Background pixels
    // remain unchanged, and the final per-output transform performs gamut map.
    float cyan = smoothstep(0.04, 0.28, source_srgb.b - source_srgb.r)
        * smoothstep(0.10, 0.55, source_srgb.g);
    float orange = smoothstep(0.05, 0.32, source_srgb.r - source_srgb.b)
        * smoothstep(0.015, 0.24, source_srgb.r - source_srgb.g);
    float accent = max(cyan, orange);
    vec3 p3 = scene_to_p3(base);
    float p3_y = p3_luminance(p3);
    // HDR changes highlight luminance, not the diffuse wallpaper chroma.
    // Keeping the SDR/P3 saturation avoids a mode-dependent orange cast.
    float saturation = 1.22;
    vec3 saturated = p3_to_scene(mix(vec3(p3_y), p3, saturation));
    vec3 graded = mix(base, saturated, accent * 0.72);

    if (u_grade_mode > 1.5) {
        vec2 d = u_texel_size * 2.0;
        float neighborhood = 0.25 * (
            luminance(srgb_to_linear(texture2D(tex, v_coords + vec2(d.x, 0.0)).rgb))
            + luminance(srgb_to_linear(texture2D(tex, v_coords - vec2(d.x, 0.0)).rgb))
            + luminance(srgb_to_linear(texture2D(tex, v_coords + vec2(0.0, d.y)).rgb))
            + luminance(srgb_to_linear(texture2D(tex, v_coords - vec2(0.0, d.y)).rgb))
        );
        float isolated = smoothstep(0.025, 0.30, y - neighborhood * 0.72);

        // The source may arrive vertically flipped on different GLES import
        // paths, so the fixed artwork masks accept both source-Y conventions.
        float logo_y_distance = min(abs(v_coords.y - 0.545), abs(v_coords.y - 0.455));
        float logo_x = smoothstep(0.32, 0.35, v_coords.x)
            * (1.0 - smoothstep(0.65, 0.68, v_coords.x));
        float logo_band = logo_x * (1.0 - smoothstep(0.055, 0.085, logo_y_distance));
        float rim_x = smoothstep(0.30, 0.35, v_coords.x);
        float rim_band = rim_x * smoothstep(0.035, 0.075, abs(v_coords.y - 0.5));

        float max_channel = max(base.r, max(base.g, base.b));
        float min_channel = min(base.r, min(base.g, base.b));
        float neutral = 1.0 - smoothstep(0.025, 0.18, max_channel - min_channel);
        float warm_pale = smoothstep(0.02, 0.22, source_srgb.r - source_srgb.b)
            * smoothstep(0.35, 0.82, source_srgb.g);

        float rim = rim_band * warm_pale * isolated;
        float logo_white = logo_band * neutral * smoothstep(0.20, 0.72, y);
        // Colored accents have dedicated targets below. Excluding them from
        // the generic highlight pass avoids promoting orange twice, which can
        // push it into the output gamut compressor and change its appearance.
        float neutral_highlight = 1.0 - accent;
        float star = (1.0 - logo_band) * (1.0 - rim) * neutral_highlight * isolated
            * smoothstep(0.035, 0.48, y);
        // The authored cyan artwork shares its hue with broad regions of the
        // blue space gradient. Requiring local isolation keeps that diffuse
        // background at its SDR luminance while still promoting cyan stars,
        // strokes, and other compact details into HDR headroom.
        float cyan_highlight = cyan * isolated;

        // Conservative HDR10: use the panel's real headroom instead of
        // inventing 800–1000 nit spectacle. DisplayHDR 400-class VA panels
        // (about 450 nits usable) clip or globally tone-map those targets.
        // Diffuse wallpaper stays at reference white; isolated accents get a modest lift.
        float usable_peak = min(u_peak_nits, u_reference_white_nits * 2.0);
        float star_nits = mix(u_reference_white_nits, usable_peak * 0.85, smoothstep(0.05, 0.85, y));
        float rim_nits = mix(u_reference_white_nits * 1.1, usable_peak, smoothstep(0.18, 0.82, y));
        float cyan_nits = mix(u_reference_white_nits, usable_peak * 0.70, smoothstep(0.03, 0.60, y));
        float orange_nits = mix(u_reference_white_nits, usable_peak * 0.75, smoothstep(0.03, 0.60, y));

        vec3 highlighted = raise_to_luminance(graded, target_level(star_nits));
        graded = mix(graded, highlighted, star);
        highlighted = raise_to_luminance(graded, target_level(rim_nits));
        graded = mix(graded, highlighted, rim);
        highlighted = raise_to_luminance(graded, target_level(cyan_nits));
        graded = mix(graded, highlighted, cyan_highlight * 0.72);
        highlighted = raise_to_luminance(graded, target_level(orange_nits));
        graded = mix(graded, highlighted, orange * isolated * 0.72);

        // Keep the large white wordmark at graphics white even though its
        // antialiased edges also look locally highlight-like.
        vec3 logo_grade = raise_to_luminance(base, target_level(u_reference_white_nits));
        graded = mix(graded, logo_grade, logo_white);

    }

    // Add the creative delta to the already-decoded opaque SDR base. Alpha is
    // intentionally zero: Smithay's premultiplied blend becomes dst + delta.
    gl_FragColor = vec4((graded - base) * alpha, 0.0);
}
"#;

const PULSE_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

uniform vec2 u_click_pos;
uniform float u_time;
uniform vec2 u_size;
uniform vec4 u_color;

varying vec2 v_coords;

void main() {
    float current_radius = u_time * 200.0;

    vec2 frag_pos = v_coords * u_size;
    vec2 d = abs(frag_pos - u_click_pos) - vec2(current_radius);
    float sdf_rect = length(max(d, 0.0)) + min(max(d.x, d.y), 0.0);

    float blur_width = 50.0;
    float alpha = 1.0 - smoothstep(-blur_width, blur_width, sdf_rect);
    alpha *= max(1.0 - (u_time / 2.0), 0.0);

    float out_alpha = max(alpha, 0.0) * u_color.a;
    gl_FragColor = vec4(u_color.rgb * out_alpha, out_alpha);
}
"#;

const ACCENT_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

uniform vec2  u_resolution;
uniform vec4  u_rect;        // x, y, w, h
uniform vec4  u_accent;      // rgba theme color
uniform float u_time;
uniform float u_pulse;       // 0.0 -> 1.0
uniform float u_active;      // 0.0 or 1.0

varying vec2 v_coords;

void main() {
    vec2 pos = v_coords * u_resolution;

    float inside_x = step(u_rect.x, pos.x) * step(pos.x, u_rect.x + u_rect.z);
    float y = pos.y - u_rect.y;
    float inside_y = step(0.0, y) * step(y, u_rect.w);
    float mask = inside_x * inside_y;

    float strip = 1.0 - smoothstep(1.8, 2.8, y);
    float glow = 1.0 - smoothstep(0.0, u_rect.w, y);
    float pulse = clamp(u_pulse, 0.0, 1.0);
    float pulse_band = (1.0 - smoothstep(0.0, 16.0, y)) * pulse;

    float alpha =
        mask * u_active * (
            strip * (0.34 + 0.22 * pulse) +
            glow * 0.08 +
            pulse_band * 0.14
        );

    float out_alpha = alpha * u_accent.a;
gl_FragColor = vec4(u_accent.rgb * out_alpha, out_alpha);
}
"#;

const FLOW_FIELD_FRAG: &str = r#"
#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

uniform vec2  u_resolution;
uniform vec4  u_rect;
uniform float u_time;
uniform float u_mode;
uniform float u_energy;
uniform vec4  u_color;

varying vec2 v_coords;

float hash11(float n) {
    return fract(sin(n) * 43758.5453123);
}

vec2 hash21(float n) {
    return vec2(hash11(n), hash11(n + 19.19));
}

float particle_count_for_mode(float mode) {
    if (mode < 0.5) {
        return 38.0;
    } else if (mode < 1.5) {
        return 64.0;
    } else if (mode < 2.5) {
        return 58.0;
    } else if (mode < 3.5) {
        return 54.0;
    }
    return 48.0;
}

vec3 palette_a(float mode, vec3 anchor) {
    if (mode < 0.5) {
        return mix(anchor, vec3(0.18, 0.48, 1.00), 0.42);
    } else if (mode < 1.5) {
        return vec3(0.18, 0.88, 1.00);
    } else if (mode < 2.5) {
        return vec3(0.16, 1.00, 0.72);
    } else if (mode < 3.5) {
        return vec3(1.00, 0.56, 0.12);
    }
    return vec3(1.00, 0.20, 0.30);
}

vec3 palette_b(float mode, vec3 anchor) {
    if (mode < 0.5) {
        return mix(anchor, vec3(0.20, 1.00, 0.94), 0.58);
    } else if (mode < 1.5) {
        return vec3(0.56, 0.30, 1.00);
    } else if (mode < 2.5) {
        return mix(anchor, vec3(0.16, 0.66, 1.00), 0.50);
    } else if (mode < 3.5) {
        return vec3(1.00, 0.90, 0.34);
    }
    return vec3(1.00, 0.48, 0.12);
}

vec3 palette_c(float mode, vec3 anchor) {
    if (mode < 0.5) {
        return mix(anchor, vec3(0.66, 0.32, 1.00), 0.52);
    } else if (mode < 1.5) {
        return vec3(1.00, 0.24, 0.76);
    } else if (mode < 2.5) {
        return vec3(1.00, 0.76, 0.22);
    } else if (mode < 3.5) {
        return vec3(1.00, 0.38, 0.64);
    }
    return vec3(1.00, 0.76, 0.52);
}

vec3 particle_color(float mode, vec2 seed, vec3 anchor) {
    vec3 a = palette_a(mode, anchor);
    vec3 b = palette_b(mode, anchor);
    vec3 c = palette_c(mode, anchor);
    float lane = fract(seed.x * 1.73 + seed.y * 0.91);
    if (lane < 0.5) {
        return mix(a, b, smoothstep(0.0, 0.5, lane));
    }
    return mix(b, c, smoothstep(0.5, 1.0, lane));
}

// A compact, divergence-like procedural field. It bends every state's base
// trajectory without requiring particle buffers or compositor-side simulation.
vec2 flow_warp(vec2 p, float time, float seed) {
    float x_wave = sin(p.y * 11.0 + time * 0.72 + seed * 6.0);
    float y_wave = cos(p.x * 9.0 - time * 0.58 + seed * 8.0);
    float cross = sin((p.x + p.y) * 7.0 + time * 0.34 + seed * 4.0);
    return vec2(x_wave + cross * 0.45, y_wave - cross * 0.45);
}

void main() {
    // The shader is clipped to the compact top-bar activity well.
    vec2 size = max(u_rect.zw, vec2(1.0));
    vec2 local = v_coords * size;
    vec2 uv = local / size;

    float mode = floor(u_mode + 0.5);
    float energy = clamp(u_energy, 0.0, 1.0);
    float count = particle_count_for_mode(mode);
    vec3 accum = vec3(0.0);

    const int PARTICLES = 64;
    for (int i = 0; i < PARTICLES; ++i) {
        float fi = float(i);
        float enabled = step(fi, count - 1.0);
        vec2 seed = hash21(fi * 13.37 + mode * 61.7);
        float phase = fract(seed.x + u_time * mix(0.018, 0.13, energy) + fi * 0.011);

        float x = 0.5;
        float y = 0.5;
        float core_scale = 1.0;
        float trail = 0.0;
        float halo = 0.0;

        if (mode < 0.5) {
            // Idle: an asymmetric, breathing nebula rather than a rigid orb.
            float drift = fract(seed.x + u_time * 0.012 + fi * 0.003);
            float wave = u_time * 0.24 + fi * 0.41 + seed.y * 5.0;
            // Wrap beyond the clipped well so a particle fades out before it
            // re-enters on the other side. Wrapping between visible edge
            // positions makes otherwise-idle particles look like they blink.
            x = mix(-0.08, 1.08, drift) + sin(wave) * 0.025;
            y = 0.50 + (seed.y - 0.5) * (0.46 - 0.16 * seed.x)
                + cos(wave * 1.17) * 0.045;
            core_scale = 1.12;
        } else if (mode < 1.5) {
            // Thinking: two temporary knots exchange particles through a curl.
            float side = step(0.5, seed.x) * 2.0 - 1.0;
            float angle = u_time * (0.70 + seed.y * 0.42) + fi * 0.67;
            float radius = 0.025 + seed.y * 0.17;
            x = 0.50 + side * 0.13 + cos(angle) * radius;
            y = 0.50 + sin(angle * 1.31) * radius * 0.84;
            x += sin(u_time * 0.46 + seed.y * 5.0) * 0.035;
            halo = 0.75;
            core_scale = 0.84;
        } else if (mode < 2.5) {
            // Working: directional ribbons make progress legible without color.
            x = mix(-0.10, 1.10, phase);
            float lane = floor(seed.y * 3.0);
            float lane_y = 0.34 + lane * 0.16;
            y = lane_y + sin(x * 8.0 + u_time * 1.35 + fi * 0.23) * 0.055;
            trail = 0.90 - abs(seed.y - 0.5) * 0.34;
            core_scale = 0.78;
        } else if (mode < 3.5) {
            // Permission wait: a slow contracting halo asks for attention.
            float angle = fi * 0.71 + seed.x * 4.0;
            float breathe = 0.76 + 0.24 * sin(u_time * 1.45 + seed.y * 2.0);
            float radius = (0.08 + seed.y * 0.29) * breathe;
            x = 0.50 + cos(angle) * radius;
            y = 0.50 + sin(angle) * radius * 0.58;
            halo = 1.0;
            core_scale = 0.88;
        } else {
            // Error: split cohesion and continuous tremor, with no flashing.
            float side = step(0.5, seed.x) * 2.0 - 1.0;
            float tremor = sin(u_time * 3.2 + fi * 1.71);
            x = 0.50 + side * (0.10 + seed.x * 0.36) + tremor * 0.018;
            y = 0.50 + (seed.y - 0.5) * 0.58
                + sin(u_time * 1.8 + fi * 0.83) * 0.035;
            trail = 0.32;
            core_scale = 0.72;
        }

        vec2 warp = flow_warp(vec2(x, y), u_time, seed.x);
        float warp_strength = mode < 0.5 ? 0.018 : (mode < 3.5 ? 0.026 : 0.010);
        x += warp.x * warp_strength;
        y += warp.y * warp_strength;

        vec2 particle = vec2(x * size.x, y * size.y);
        vec2 delta = local - particle;
        float dist = length(delta);
        float core = exp(-(dist * dist) / (mix(42.0, 18.0, energy) * core_scale));
        float streak = exp(-abs(delta.y) / mix(4.8, 2.4, energy))
            * exp(-max(delta.x, 0.0) / 16.0) * trail;
        float ring = exp(-pow((dist - size.y * 0.13) / max(size.y * 0.08, 1.0), 2.0));
        vec3 color = particle_color(mode, seed, u_color.rgb);
        // Motion carries the activity signal. Keeping particle luminance stable
        // avoids an unrelated sparkle/flicker when the field is otherwise idle.
        accum += color * enabled * (core + streak * 0.34 + ring * halo * 0.08);
    }

    // Exponential mapping keeps overlapping particles colorful instead of white.
    // A little extra exposure prevents theme-colored particles from sinking into
    // similarly hued chrome, while the shaped alpha keeps the cloud readable at
    // its softer edges after premultiplied blending.
    vec3 mapped = vec3(1.0) - exp(-accum * mix(0.24, 0.42, energy));
    float luminance = dot(mapped, vec3(0.2126, 0.7152, 0.0722));
    float boundary = smoothstep(0.0, 0.06, uv.x)
        * smoothstep(0.0, 0.06, 1.0 - uv.x)
        * smoothstep(0.0, 0.12, uv.y)
        * smoothstep(0.0, 0.12, 1.0 - uv.y);
    float alpha = smoothstep(0.008, 0.28, luminance) * 0.96
        * boundary * u_color.a;
    gl_FragColor = vec4(mapped * alpha, alpha);
}
"#;

const SCREENSAVER_FRAG: &str = r#"
#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

varying vec2 v_coords;

uniform vec2 u_resolution;
uniform float u_time;

float hash11(float p) {
    return fract(sin(p) * 43758.5453123);
}

float hash21(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}

vec2 hash22(vec2 p) {
    float n = dot(p, vec2(127.1, 311.7));
    return fract(sin(vec2(n, n + 19.19)) * 43758.5453123);
}

vec3 palette(float t) {
    vec3 a = vec3(0.02, 0.04, 0.08);
    vec3 b = vec3(0.12, 0.18, 0.34);
    vec3 c = vec3(0.88, 0.72, 0.52);
    vec3 d = vec3(0.18, 0.32, 0.56);
    return a + b * cos(6.28318 * (c * t + d));
}

void main() {
    vec2 uv = v_coords;
    vec2 aspect = vec2(u_resolution.x / max(u_resolution.y, 1.0), 1.0);
    vec2 p = (uv - 0.5) * aspect;

    float t = u_time;
    vec2 drift = vec2(
        0.19 * sin(t * 0.17) + 0.05 * sin(t * 0.73),
        0.14 * cos(t * 0.13) + 0.05 * cos(t * 0.61)
    );
    vec2 center = drift;
    vec2 to_center = p - center;
    float dist = length(to_center);
    float angle = atan(to_center.y, to_center.x);

    // Background nebula gradient.
    float nebula = 0.18 + 0.14 * sin(t * 0.05 + uv.x * 4.0 + uv.y * 2.0);
    vec3 color = vec3(0.01, 0.015, 0.03) + palette(nebula) * 0.12;

    // Starfield: three parallax layers with different cell sizes and twinkle rates.
    for (int layer = 0; layer < 3; ++layer) {
        float lf = float(layer);
        float cell = mix(0.085, 0.22, lf / 2.0);
        vec2 grid = floor((uv + vec2(t * 0.0015 * (lf + 1.0), t * 0.0010 * (lf + 1.0))) / cell);
        vec2 rnd = hash22(grid + lf * 31.7);
        vec2 star_pos = (grid + rnd) * cell;
        vec2 delta = uv - star_pos;
        float twinkle = 0.55 + 0.45 * sin(t * (1.6 + lf * 0.7) + hash21(grid) * 6.28318);
        float radius = mix(0.002, 0.0055, lf / 2.0);
        float star = exp(-dot(delta, delta) / (radius * radius));
        float hue = hash21(grid + 8.3);
        vec3 star_color = mix(vec3(0.78, 0.88, 1.0), vec3(1.0, 0.84, 0.58), hue);
        color += star_color * star * twinkle * (1.05 - 0.2 * lf);
    }

    // Gravitational lensing around the black hole.
    float lens = 0.022 / max(dist * dist, 0.010);
    vec2 warped = p + normalize(to_center + vec2(1e-4)) * lens * 0.06;
    float warped_dist = length(warped - center);
    float hole = smoothstep(0.030, 0.0, warped_dist);

    // Accretion disk: tilted ring with turbulence.
    float disk_r = 0.17 + 0.01 * sin(t * 0.33);
    float disk_width = 0.035;
    float ring = exp(-pow((warped_dist - disk_r) / disk_width, 2.0));
    float spokes = 0.55 + 0.45 * sin(angle * 10.0 - t * 2.8 + warped_dist * 26.0);
    float turbulence = 0.7 + 0.3 * sin(warped_dist * 64.0 + t * 6.0);
    vec3 disk_color = mix(vec3(0.95, 0.62, 0.18), vec3(0.42, 0.18, 0.82), 0.32 + 0.68 * hash11(floor(t)));
    color += disk_color * ring * spokes * turbulence * 1.15;

    // A smaller trailing wake gives the whole thing some motion even when the ring is centered.
    float wake = exp(-pow((warped_dist - 0.245) / 0.085, 2.0));
    wake *= 0.5 + 0.5 * sin(angle * 6.0 + t * 1.4);
    color += vec3(0.18, 0.35, 0.75) * wake * 0.20;

    // Event horizon and vignette.
    color *= 1.0 - hole * 0.94;
    float vignette = smoothstep(1.12, 0.22, length(p));
    color *= vignette;

    // Tiny motion along the neighborhood path so the center never sits still.
    vec2 neighborhood = vec2(
        0.05 * sin(t * 0.47 + 1.4),
        0.03 * cos(t * 0.39 + 0.6)
    );
    float neighborhood_pull = exp(-pow(length(p - neighborhood) / 0.28, 2.0));
    color += vec3(0.10, 0.12, 0.18) * neighborhood_pull * 0.12;

    gl_FragColor = vec4(clamp(color, 0.0, 1.0), 1.0);
}
"#;

const GLASS_CONTROL_FRAG: &str = r#"
//_DEFINES

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : enable
#endif

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform sampler2D u_background;
uniform float alpha;

varying vec2 v_coords;

uniform vec2 u_size;
uniform vec2 u_background_uv_size;
uniform vec2 u_icon_uv_origin;
uniform vec2 u_icon_uv_size;
uniform vec4 u_icon_rect;
uniform vec2 u_icon_texel_size;
uniform vec4 u_glass_tint;
uniform vec3 u_accent_color;
uniform float u_corner_radius;
uniform float u_border_width;
uniform float u_hover;
uniform float u_pressed;
uniform float u_enabled;
uniform float u_active;
uniform float u_warning;
uniform vec3 u_light_dir;
uniform float u_opacity;
uniform float u_output_factor;
uniform float u_icon_strength;
uniform float u_etch_depth;

float rounded_rect_sdf(vec2 point, vec2 half_size, float radius) {
    vec2 q = abs(point) - half_size + vec2(radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - radius;
}

float icon_sample(vec2 local_uv) {
    vec2 icon_uv = (local_uv - u_icon_rect.xy) / max(u_icon_rect.zw, vec2(0.0001));
    float inside = step(0.0, icon_uv.x) * step(0.0, icon_uv.y)
        * step(icon_uv.x, 1.0) * step(icon_uv.y, 1.0);
    vec2 atlas_uv = u_icon_uv_origin + clamp(icon_uv, 0.0, 1.0) * u_icon_uv_size;
    return texture2D(tex, atlas_uv).a * inside;
}

void main() {
    vec2 uv = v_coords;
    vec2 p = (uv - vec2(0.5)) * u_size;
    float radius = min(u_corner_radius, min(u_size.x, u_size.y) * 0.5);
    float distance_to_button = rounded_rect_sdf(p, u_size * 0.5, radius);
    float edge_aa = 1.0;
    float button_alpha = 1.0 - smoothstep(-edge_aa, edge_aa, distance_to_button);
    if (button_alpha <= 0.0) {
        discard;
    }

    // The compositor's Transform::Normal offscreen projection and the copied texture
    // use the same top-to-bottom local coordinate convention.
    vec2 background_texture_size = u_size / max(u_background_uv_size, vec2(0.0001));
    vec2 background_uv = (vec2(0.5) + uv
        * max(u_size - vec2(1.0), vec2(0.0))) / background_texture_size;
    vec3 background = texture2D(u_background, background_uv).rgb;
    float hover = clamp(u_hover, 0.0, 1.0);
    float pressed = clamp(u_pressed, 0.0, 1.0);
    float enabled = clamp(u_enabled, 0.0, 1.0);
    float active = clamp(u_active, 0.0, 1.0);
    float warning = clamp(u_warning, 0.0, 1.0);

    float glass_mix = clamp(u_glass_tint.a + hover * 0.05 + pressed * 0.04, 0.0, 1.0);
    vec3 glass_color = mix(background, u_glass_tint.rgb, glass_mix);
    float top_highlight = smoothstep(0.55, 0.0, uv.y)
        * smoothstep(0.0, 0.25, uv.x) * smoothstep(1.0, 0.75, uv.x);
    glass_color += vec3(0.10) * top_highlight;
    glass_color += vec3(0.025) * (1.0 - uv.y);
    glass_color -= vec3(0.020) * uv.y;

    float border = smoothstep(-edge_aa, edge_aa, distance_to_button + u_border_width)
        * button_alpha;
    vec3 border_color = mix(vec3(0.35), vec3(0.75), top_highlight)
        + u_accent_color * active * 0.15;
    glass_color = mix(glass_color, border_color, border * 0.30);

    // Convert one atlas texel to button-local UV before taking mask derivatives.
    vec2 local_texel = u_icon_texel_size / max(u_icon_uv_size, vec2(0.0001))
        * u_icon_rect.zw;
    float center = icon_sample(uv);
    float left = icon_sample(uv - vec2(local_texel.x, 0.0));
    float right = icon_sample(uv + vec2(local_texel.x, 0.0));
    float up = icon_sample(uv - vec2(0.0, local_texel.y));
    float down = icon_sample(uv + vec2(0.0, local_texel.y));
    vec2 gradient = vec2(right - left, down - up);
    vec3 light_direction = normalize(u_light_dir);
    vec3 icon_normal = normalize(vec3(-gradient * u_etch_depth, 1.0));
    float diffuse = max(dot(icon_normal, light_direction), 0.0);
    vec2 light_xy = normalize(light_direction.xy + vec2(0.0001));
    float directional_edge = dot(normalize(gradient + vec2(0.0001)), light_xy);
    float etched_highlight = max(-directional_edge, 0.0);
    float etched_shadow = max(directional_edge, 0.0);
    float icon_edge = clamp(length(gradient) * 2.5, 0.0, 1.0);
    float surface_luminance = dot(glass_color, vec3(0.2126, 0.7152, 0.0722));
    vec3 icon_contrast = mix(
        vec3(0.94, 0.96, 1.0),
        vec3(0.10, 0.08, 0.12),
        smoothstep(0.40, 0.62, surface_luminance)
    );
    vec3 icon_base = mix(icon_contrast, u_accent_color, active * 0.40);
    icon_base *= mix(0.55, 1.0, enabled);

    glass_color += u_accent_color * active * center * (0.20 + hover * 0.10);
    glass_color += mix(u_accent_color, vec3(1.0, 0.35, 0.08), warning)
        * warning * center * 0.35;
    glass_color = mix(glass_color, icon_base, center * u_icon_strength);
    glass_color += vec3(0.36) * etched_highlight * icon_edge * center;
    glass_color -= vec3(0.28) * etched_shadow * icon_edge * center;
    glass_color += vec3(0.06) * diffuse * icon_edge;
    glass_color *= mix(1.0, 0.90, pressed);
    glass_color += u_accent_color * border * hover * 0.08;

    float luminance = dot(glass_color, vec3(0.2126, 0.7152, 0.0722));
    glass_color = mix(vec3(luminance), glass_color, enabled);
    // The captured background is already part of glass_color, so replace the covered
    // framebuffer pixel instead of blending the same background into it twice.
    float effect_mix = clamp(u_opacity * u_output_factor, 0.0, 1.0);
    vec3 final_color = mix(background, glass_color, effect_mix);
    float coverage = button_alpha * alpha;
    gl_FragColor = vec4(final_color * coverage, coverage);
}
"#;
