// crates/focaldesk-engine/src/core/chrome_shader.rs
use focaldesk_logging::flog;

use smithay::backend::renderer::gles::GlesTexProgram;
use smithay::backend::renderer::gles::{
    GlesError, GlesPixelProgram, GlesRenderer, UniformName, UniformType,
};

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
    pub srgb_to_linear: Option<GlesTexProgram>,
    pub linear_to_srgb: Option<GlesTexProgram>,
    pub composite_linear_layer: Option<GlesTexProgram>,
    pub pulse: Option<GlesPixelProgram>,
    pub accent: Option<GlesPixelProgram>,
}

impl Default for ChromeShaders {
    fn default() -> Self {
        Self::new()
    }
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
            srgb_to_linear: None,
            linear_to_srgb: None,
            composite_linear_layer: None,
            pulse: None,
            accent: None,
        }
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

        if self.font_text.is_none() {
            self.font_text = Some(renderer.compile_custom_texture_shader(
                FONT_TEXT_FRAG,
                &[UniformName::new("u_tint", UniformType::_4f)],
            )?);
        }

        if self.wallpaper_tint.is_none() {
            self.wallpaper_tint = Some(renderer.compile_custom_texture_shader(
                WALLPAPER_TINT_FRAG,
                &[UniformName::new("u_tint", UniformType::_4f)],
            )?);
        }

        if self.srgb_to_linear.is_none() {
            self.srgb_to_linear =
                Some(renderer.compile_custom_texture_shader(SRGB_TO_LINEAR_FRAG, &[])?);
        }

        if self.linear_to_srgb.is_none() {
            self.linear_to_srgb =
                Some(renderer.compile_custom_texture_shader(LINEAR_TO_SRGB_FRAG, &[])?);
        }

        if self.composite_linear_layer.is_none() {
            self.composite_linear_layer =
                Some(renderer.compile_custom_texture_shader(COMPOSITE_LINEAR_LAYER_FRAG, &[])?);
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

uniform vec4 u_face_color;
uniform vec4 u_light_color;
uniform vec4 u_shadow_color;
uniform vec4 u_glow_color;

float hash(float n) {
    return fract(sin(n) * 43758.5453123);
}

void main() {
    vec2 p = v_coords * size;

    float dl = p.x;
    float dt = p.y;
    float dr = size.x - p.x;
    float db = size.y - p.y;

    float edge_dist = min(min(dl, dr), min(dt, db));

    float bevel = max(u_bevel, 0.0001);
    float soft  = max(u_softness, 0.0001);

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

    gl_FragColor = vec4(color, u_face_color.a * alpha);
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

varying vec2 v_uv;

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
    vec2 p = v_uv * u_size;

    float radius = u_bevel;
    float sdf = rounded_box_sdf(p, u_size, radius);

    float edge = smoothstep(u_softness, 0.0, abs(sdf));

    // Inner shadow for recessed look
    float inner = smoothstep(0.0, u_softness, -sdf);

    vec3 color = u_face_color.rgb;

    // Recess shadow
    color -= inner * u_inner_shadow * u_shadow_color.rgb;

    // Center glow / backlight
    vec2 centered = v_uv - vec2(0.5);
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

varying vec2 v_uv;

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
    vec2 p = v_uv * u_size;
    vec2 center = u_size * 0.5;
    vec2 local = p - center;

    float sdf = rounded_box_sdf(local, u_size * 0.5, u_radius);

    // main mask
    float alpha = 1.0 - smoothstep(0.0, u_softness, sdf);

    // outer edge / bevel response
    float edge_band = 1.0 - smoothstep(u_bevel, u_bevel + u_softness, abs(sdf));

    // top reflection band
    float top_band = (1.0 - smoothstep(0.0, 0.22, v_uv.y)) * u_highlight_strength;

    // soft lower inner shadow
    float bottom_shadow = smoothstep(0.72, 1.0, v_uv.y) * u_shadow_strength;

    // faint horizontal material sweep so it does not feel dead flat
    float horiz = 0.5 + 0.5 * cos((v_uv.x - 0.5) * 3.14159);
    float face_variation = 0.035 * horiz;

    // trim line near top
    float trim_mask = 1.0 - smoothstep(
        u_trim_height,
        u_trim_height + max(1.0 / max(u_size.y, 1.0), 0.001),
        v_uv.y
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

const TINTED_ICON_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

varying vec2 v_coords;

uniform sampler2D tex;
uniform vec4 u_tint;

void main() {
    vec4 src = texture2D(tex, v_coords);

    if (src.a < 0.01) {
        discard;
    }

    gl_FragColor = vec4(u_tint.rgb, src.a * u_tint.a);
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
precision highp float;
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
    // Wayland alpha buffers are premultiplied. Decode the straight color, then
    // premultiply again so Smithay's ONE/ONE_MINUS_SRC_ALPHA blend remains valid.
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
precision highp float;
#endif

varying vec2 v_coords;
uniform float alpha;

vec3 linear_to_srgb(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.0031308));
    vec3 low = c * 12.92;
    vec3 high = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, vec3(cutoff));
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
    gl_FragColor = vec4(linear_to_srgb(straight) * src.a, src.a) * alpha;
}
"#;

const LINEAR_TO_SRGB_FRAG: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

#ifdef GL_ES
precision highp float;
#endif

varying vec2 v_coords;
uniform float alpha;

vec3 linear_to_srgb(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.0031308));
    vec3 low = c * 12.92;
    vec3 high = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, vec3(cutoff));
}

void main() {
    vec4 src = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    src.a = 1.0;
#endif
    vec3 straight = src.a > 0.0 ? max(src.rgb / src.a, vec3(0.0)) : vec3(0.0);
    gl_FragColor = vec4(linear_to_srgb(straight) * src.a, src.a) * alpha;
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

varying vec2 v_coords;

void main() {
    vec4 src = texture2D(tex, v_coords);

    vec3 rgb = mix(src.rgb, u_tint.rgb, u_tint.a);

    gl_FragColor = vec4(rgb, src.a);
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
