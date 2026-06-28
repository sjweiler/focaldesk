//! Color descriptions and reference transfer functions used by the compositor.
//!
//! Phase A/B: scene-linear Rec.709 working space, relative gamut mapping.
//! Phase C1: SDR scanout encode uses each output's ICC/EDID description.
//! Phase C2: colord D-Bus hotplug/profile refresh (`core::colord`).

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::{Cacheable, SurfaceData};
use focaldesk_settings_core::DisplayColorProfile;

/// CIE 1931 xy chromaticities for RGB primaries + D65 white.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrimariesChromaticity {
    pub r: [f32; 2],
    pub g: [f32; 2],
    pub b: [f32; 2],
    pub w: [f32; 2],
}

/// Pixel color primaries attached to a client surface or output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorPrimaries {
    Srgb,
    DisplayP3,
    Bt2020,
    Custom(PrimariesChromaticity),
}

impl Default for ColorPrimaries {
    fn default() -> Self {
        Self::Srgb
    }
}

impl ColorPrimaries {
    pub fn chromaticity(self) -> PrimariesChromaticity {
        match self {
            Self::Srgb => PrimariesChromaticity::SRGB,
            Self::DisplayP3 => PrimariesChromaticity::DISPLAY_P3,
            Self::Bt2020 => PrimariesChromaticity::BT2020,
            Self::Custom(c) => c,
        }
    }

    pub fn from_wp_named(
        primaries: wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::Primaries,
    ) -> Option<Self> {
        use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::Primaries;
        match primaries {
            Primaries::Srgb => Some(Self::Srgb),
            Primaries::DisplayP3 => Some(Self::DisplayP3),
            Primaries::Bt2020 => Some(Self::Bt2020),
            _ => None,
        }
    }
}

impl PrimariesChromaticity {
    pub const SRGB: Self = Self {
        r: [0.640, 0.330],
        g: [0.300, 0.600],
        b: [0.150, 0.060],
        w: [0.3127, 0.3290],
    };
    pub const DISPLAY_P3: Self = Self {
        r: [0.680, 0.320],
        g: [0.265, 0.690],
        b: [0.150, 0.060],
        w: [0.3127, 0.3290],
    };
    pub const BT2020: Self = Self {
        r: [0.708, 0.292],
        g: [0.170, 0.797],
        b: [0.131, 0.046],
        w: [0.3127, 0.3290],
    };
}

/// True when all CIE xy coordinates are finite and within [0, 1].
pub fn chromaticity_is_valid(ch: &PrimariesChromaticity) -> bool {
    let in_range = |xy: [f32; 2]| {
        xy[0].is_finite()
            && xy[1].is_finite()
            && (0.0..=1.0).contains(&xy[0])
            && (0.0..=1.0).contains(&xy[1])
    };
    in_range(ch.r) && in_range(ch.g) && in_range(ch.b) && in_range(ch.w)
}

/// Rejects coordinates that fit [0,1] but are far too small for any real display (Chrome wire bugs).
pub fn primaries_plausible(ch: &PrimariesChromaticity) -> bool {
    if !chromaticity_is_valid(ch) {
        return false;
    }
    let peak = [ch.r, ch.g, ch.b, ch.w]
        .into_iter()
        .flatten()
        .fold(0.0f32, f32::max);
    peak >= 0.15
}

/// Electrical-to-optical transfer function attached to a client surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferFunction {
    #[default]
    Srgb,
    /// Piecewise sRGB / BT.1886 for SDR video-style content.
    Bt1886,
    /// Simple gamma 2.2 power law.
    Gamma22,
    Linear,
    /// SMPTE ST 2084 perceptual quantizer (HDR PQ scanout).
    St2084Pq,
}

/// Shader decode mode sent as `u_decode_tf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TransferDecodeMode {
    SrgbPiecewise = 0,
    LinearPassThrough = 1,
    Gamma22 = 2,
    St2084Pq = 3,
}

impl TransferFunction {
    pub fn decode_mode(self) -> TransferDecodeMode {
        match self {
            Self::Srgb | Self::Bt1886 => TransferDecodeMode::SrgbPiecewise,
            Self::Gamma22 => TransferDecodeMode::Gamma22,
            Self::Linear => TransferDecodeMode::LinearPassThrough,
            Self::St2084Pq => TransferDecodeMode::St2084Pq,
        }
    }

    /// Electrical encoding applied when writing the KMS framebuffer.
    pub fn encode_mode(self) -> TransferDecodeMode {
        match self {
            Self::Srgb | Self::Bt1886 => TransferDecodeMode::SrgbPiecewise,
            Self::Gamma22 => TransferDecodeMode::Gamma22,
            Self::Linear => TransferDecodeMode::SrgbPiecewise,
            Self::St2084Pq => TransferDecodeMode::St2084Pq,
        }
    }
}

/// Color interpretation of a surface buffer or output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorDescription {
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
    pub reference_white_nits: f32,
    pub max_luminance_nits: f32,
    pub max_cll_nits: Option<f32>,
    pub max_fall_nits: Option<f32>,
}

impl ColorDescription {
    /// Wayland surfaces without an image description are sRGB by definition.
    pub const SRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Srgb,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
    };

    pub const LINEAR_SRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Linear,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
    };

    /// Windows-scRGB (`create_windows_scrgb`): sRGB primaries, extended linear, 203 nits ref white.
    pub const WINDOWS_SCRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Linear,
        reference_white_nits: 203.0,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
    };

    pub const DISPLAY_P3_SRGB: Self = Self {
        primaries: ColorPrimaries::DisplayP3,
        transfer: TransferFunction::Srgb,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
    };

    /// BT.2020 + PQ from EDID Type-1 static metadata (HDR scanout target).
    pub fn bt2020_pq_hdr(max_luminance_nits: f32, max_fall_nits: f32) -> Self {
        Self {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::St2084Pq,
            reference_white_nits: 203.0,
            max_luminance_nits,
            max_cll_nits: Some(max_luminance_nits),
            max_fall_nits: Some(max_fall_nits),
        }
    }
}

impl Default for ColorDescription {
    fn default() -> Self {
        Self::SRGB
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderingIntent {
    #[default]
    Perceptual,
    Relative,
    Absolute,
}

/// Committed color state used when drawing a client surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceColorRenderState {
    pub description: ColorDescription,
    pub intent: RenderingIntent,
    /// Row-major 3×3: linear client RGB → scene-linear Rec.709.
    pub client_to_scene: [[f32; 3]; 3],
}

impl SurfaceColorRenderState {
    pub fn for_description(description: ColorDescription, intent: RenderingIntent) -> Self {
        let client_to_scene =
            gamut_matrix_linear_rgb(description.primaries, scene_working_primaries(), intent);
        Self {
            description,
            intent,
            client_to_scene,
        }
    }

    pub fn srgb_default() -> Self {
        Self::for_description(ColorDescription::SRGB, RenderingIntent::Perceptual)
    }
}

/// Double-buffered color state associated with a `wl_surface`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceColorState {
    pub description: Option<ColorDescription>,
    pub intent: RenderingIntent,
}

impl SurfaceColorState {
    pub fn effective_description(&self) -> ColorDescription {
        self.description.unwrap_or(ColorDescription::SRGB)
    }

    pub fn render_state(&self) -> SurfaceColorRenderState {
        SurfaceColorRenderState::for_description(self.effective_description(), self.intent)
    }
}

impl Default for SurfaceColorState {
    fn default() -> Self {
        Self {
            description: None,
            intent: RenderingIntent::Perceptual,
        }
    }
}

impl Cacheable for SurfaceColorState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Scene compositing space: linear-light Rec.709 (same primaries as sRGB).
pub fn scene_working_primaries() -> ColorPrimaries {
    ColorPrimaries::Srgb
}

/// Default SDR output description for an output when no ICC/EDID data is available.
pub fn default_output_color_description() -> ColorDescription {
    ColorDescription::SRGB
}

/// Apply a user-selected output profile override on top of the resolved base output description.
pub fn apply_output_color_profile_override(
    base: ColorDescription,
    override_profile: DisplayColorProfile,
) -> ColorDescription {
    match override_profile {
        DisplayColorProfile::Auto => base,
        DisplayColorProfile::Srgb => ColorDescription::SRGB,
        DisplayColorProfile::DisplayP3 => ColorDescription::DISPLAY_P3_SRGB,
    }
}

/// Color description used when encoding the KMS framebuffer for a given output.
///
/// Uses the output's ICC/EDID profile (same data advertised via `wp_color_management_v1`).
/// When HDR is active, returns BT.2020 + PQ from EDID metadata instead of ICC SDR.
pub fn kms_scanout_encode_description(
    output: ColorDescription,
    hdr_active: bool,
    hdr_max_luminance_nits: Option<f32>,
    hdr_max_fall_nits: Option<f32>,
) -> ColorDescription {
    if hdr_active {
        if let (Some(max), Some(fall)) = (hdr_max_luminance_nits, hdr_max_fall_nits) {
            return ColorDescription::bt2020_pq_hdr(max, fall);
        }
    }
    output
}

/// Whether the finished scene sRGB buffer needs a full-frame output encode pass.
pub fn output_encode_scanout_needed(
    description: ColorDescription,
    icc_lut: Option<&crate::core::icc_lut::OutputIccLut>,
) -> bool {
    if icc_lut.is_some() && crate::core::icc_lut::icc_lut_shader_enabled() {
        return true;
    }
    if matches!(
        std::env::var("FOCALDESK_OUTPUT_ENCODE").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    ) {
        return false;
    }
    description.primaries != ColorPrimaries::Srgb
        || !matches!(
            description.transfer,
            TransferFunction::Srgb | TransferFunction::Bt1886
        )
}

/// Row-major 3×3 matrix: linear `src` RGB → linear `dst` RGB.
pub fn gamut_matrix_linear_rgb(
    src: ColorPrimaries,
    dst: ColorPrimaries,
    intent: RenderingIntent,
) -> [[f32; 3]; 3] {
    let _ = intent;
    // Phase A: perceptual/absolute fall back to relative colorimetric clipping.
    let m_src = primaries_to_rgb_to_xyz(src.chromaticity());
    let m_dst = primaries_to_rgb_to_xyz(dst.chromaticity());
    let inv_dst = invert_3x3(m_dst);
    multiply_3x3(inv_dst, m_src)
}

/// Row-major 3×3: scene-linear Rec.709 → linear output primaries.
pub fn scene_to_output_matrix(output: ColorDescription, intent: RenderingIntent) -> [[f32; 3]; 3] {
    gamut_matrix_linear_rgb(scene_working_primaries(), output.primaries, intent)
}

fn primaries_to_rgb_to_xyz(ch: PrimariesChromaticity) -> [[f32; 3]; 3] {
    let xr = xy_to_xyz(ch.r);
    let xg = xy_to_xyz(ch.g);
    let xb = xy_to_xyz(ch.b);
    let xw = xy_to_xyz(ch.w);

    let s = invert_3x3([
        [xr[0], xg[0], xb[0]],
        [xr[1], xg[1], xb[1]],
        [xr[2], xg[2], xb[2]],
    ]);
    let sr = s[0][0] * xw[0] + s[0][1] * xw[1] + s[0][2] * xw[2];
    let sg = s[1][0] * xw[0] + s[1][1] * xw[1] + s[1][2] * xw[2];
    let sb = s[2][0] * xw[0] + s[2][1] * xw[1] + s[2][2] * xw[2];

    [
        [xr[0] * sr, xg[0] * sg, xb[0] * sb],
        [xr[1] * sr, xg[1] * sg, xb[1] * sb],
        [xr[2] * sr, xg[2] * sg, xb[2] * sb],
    ]
}

fn xy_to_xyz(xy: [f32; 2]) -> [f32; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

fn multiply_3x3(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn invert_3x3(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv_det = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ]
}

pub fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

/// When false, keep the legacy single-pass sRGB offscreen path even if FP16 is available.
pub fn linear_sdr_runtime_enabled() -> bool {
    !matches!(
        std::env::var("FOCALDESK_LINEAR_SDR").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Debug/testing hook: treat every committed surface as linear-encoded.
pub fn force_linear_surfaces() -> bool {
    matches!(
        std::env::var("FOCALDESK_FORCE_LINEAR_SURFACES")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// When true, composite HDR PQ offscreen buffers (C3b userspace path; KMS apply is C3c).
pub fn hdr_render_runtime_enabled() -> bool {
    matches!(
        std::env::var("FOCALDESK_HDR_RENDER").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// When true, apply HDR connector properties and 10-bit scanout (C3c).
pub fn hdr_kms_env_blocked() -> bool {
    matches!(
        std::env::var("FOCALDESK_HDR").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Explicit env force for KMS HDR (optional; settings `hdr_requested` is sufficient).
pub fn hdr_kms_env_forced() -> bool {
    matches!(
        std::env::var("FOCALDESK_HDR").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Whether the DRM loop may attempt live KMS HDR commits.
pub fn hdr_runtime_may_apply_kms(any_output_hdr_requested: bool) -> bool {
    if hdr_kms_env_blocked() {
        return false;
    }
    hdr_kms_env_forced() || any_output_hdr_requested
}

/// HDR is live on this output: KMS connector/scanout matches PQ encode.
pub fn output_hdr_render_active(
    hdr_requested: bool,
    hdr_supported: bool,
    hdr_kms_applied: bool,
) -> bool {
    if hdr_kms_env_blocked() || !hdr_requested || !hdr_supported {
        return false;
    }
    hdr_kms_applied
}

/// C3b lab mode: PQ encode without KMS (`FOCALDESK_HDR_RENDER=1` only). Image may look wrong.
pub fn output_hdr_pq_test_encode_active(
    hdr_requested: bool,
    hdr_supported: bool,
    hdr_kms_applied: bool,
) -> bool {
    hdr_requested && hdr_supported && hdr_render_runtime_enabled() && !hdr_kms_applied
}

/// When false, do not advertise `wp_color_management_v1`.
pub fn wp_color_management_enabled() -> bool {
    !matches!(
        std::env::var("FOCALDESK_WP_COLOR").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// When false, `wp_color` advertises canonical sRGB instead of ICC/wide-gamut primaries.
/// Defaults on when the ICC LUT shader is available; set `FOCALDESK_WP_COLOR_WIDE=0` to disable.
pub fn wp_color_wide_gamut_enabled(lut_shader_available: bool) -> bool {
    match std::env::var("FOCALDESK_WP_COLOR_WIDE").ok().as_deref() {
        Some("0") | Some("false") | Some("no") | Some("off") => false,
        Some("1") | Some("true") | Some("yes") => true,
        _ => lut_shader_available,
    }
}

pub fn effective_surface_render_state(
    states: &SurfaceData,
    force_linear: bool,
) -> SurfaceColorRenderState {
    if force_linear {
        return SurfaceColorRenderState::for_description(
            ColorDescription::LINEAR_SRGB,
            RenderingIntent::Perceptual,
        );
    }
    states
        .cached_state
        .get::<SurfaceColorState>()
        .current()
        .render_state()
}

/// Legacy helper — transfer only.
pub fn effective_transfer(states: &SurfaceData, force_linear: bool) -> TransferFunction {
    effective_surface_render_state(states, force_linear)
        .description
        .transfer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "actual={actual} expected={expected} tolerance={tolerance}"
        );
    }

    #[test]
    fn untagged_surface_defaults_to_srgb() {
        assert_eq!(ColorDescription::default(), ColorDescription::SRGB);
        assert_eq!(
            SurfaceColorState::default().effective_description(),
            ColorDescription::SRGB
        );
    }

    #[test]
    fn srgb_decode_matches_reference_points() {
        close(srgb_to_linear(0.0), 0.0, 1e-7);
        close(srgb_to_linear(0.04045), 0.0031308, 1e-6);
        close(srgb_to_linear(1.0), 1.0, 1e-7);
    }

    #[test]
    fn srgb_transfer_round_trips() {
        for encoded in [0.0, 0.003, 0.04045, 0.18, 0.5, 1.0] {
            close(linear_to_srgb(srgb_to_linear(encoded)), encoded, 1e-6);
        }
    }

    #[test]
    fn output_encode_skipped_for_default_srgb_output() {
        assert!(!output_encode_scanout_needed(ColorDescription::SRGB, None));
    }

    #[test]
    fn output_encode_needed_for_display_p3_monitor() {
        assert!(output_encode_scanout_needed(
            ColorDescription::DISPLAY_P3_SRGB,
            None
        ));
    }

    #[test]
    fn kms_scanout_encode_uses_output_description() {
        let p3 = ColorDescription::DISPLAY_P3_SRGB;
        assert_eq!(kms_scanout_encode_description(p3, false, None, None), p3);
    }

    #[test]
    fn kms_scanout_encode_returns_bt2020_pq_when_hdr_active() {
        let sdr = ColorDescription::SRGB;
        let hdr = kms_scanout_encode_description(sdr, true, Some(600.0), Some(400.0));
        assert_eq!(hdr.primaries, ColorPrimaries::Bt2020);
        assert_eq!(hdr.transfer, TransferFunction::St2084Pq);
        assert_eq!(hdr.max_luminance_nits, 600.0);
    }

    #[test]
    fn hdr_render_active_follows_settings_and_kms() {
        std::env::remove_var("FOCALDESK_HDR_RENDER");
        std::env::remove_var("FOCALDESK_HDR");
        assert!(!output_hdr_render_active(true, true, false));
        assert!(output_hdr_render_active(true, true, true));
        assert!(!output_hdr_pq_test_encode_active(true, true, true));

        std::env::set_var("FOCALDESK_HDR_RENDER", "1");
        assert!(output_hdr_pq_test_encode_active(true, true, false));
        assert!(!output_hdr_pq_test_encode_active(true, true, true));
        std::env::remove_var("FOCALDESK_HDR_RENDER");

        std::env::set_var("FOCALDESK_HDR", "0");
        assert!(!output_hdr_render_active(true, true, true));
        assert!(!hdr_runtime_may_apply_kms(true));
        std::env::remove_var("FOCALDESK_HDR");

        assert!(hdr_runtime_may_apply_kms(true));
        assert!(!output_hdr_render_active(false, true, true));
        assert!(!output_hdr_render_active(true, false, true));
    }

    #[test]
    fn wp_color_wide_gamut_defaults_on_with_lut() {
        std::env::remove_var("FOCALDESK_WP_COLOR_WIDE");
        assert!(!wp_color_wide_gamut_enabled(false));
        assert!(wp_color_wide_gamut_enabled(true));
        std::env::set_var("FOCALDESK_WP_COLOR_WIDE", "0");
        assert!(!wp_color_wide_gamut_enabled(true));
        std::env::set_var("FOCALDESK_WP_COLOR_WIDE", "1");
        assert!(wp_color_wide_gamut_enabled(false));
        std::env::remove_var("FOCALDESK_WP_COLOR_WIDE");
    }

    #[test]
    fn display_p3_output_produces_non_identity_scene_matrix() {
        let m =
            scene_to_output_matrix(ColorDescription::DISPLAY_P3_SRGB, RenderingIntent::Relative);
        let identity = m[0][0] == 1.0
            && m[0][1] == 0.0
            && m[0][2] == 0.0
            && m[1][0] == 0.0
            && m[1][1] == 1.0
            && m[1][2] == 0.0
            && m[2][0] == 0.0
            && m[2][1] == 0.0
            && m[2][2] == 1.0;
        assert!(!identity, "Display P3 output should remap scene primaries");
    }

    #[test]
    fn identity_gamut_matrix_for_matching_primaries() {
        let m = gamut_matrix_linear_rgb(
            ColorPrimaries::Srgb,
            ColorPrimaries::Srgb,
            RenderingIntent::Relative,
        );
        for i in 0..3 {
            for j in 0..3 {
                let expected = if i == j { 1.0 } else { 0.0 };
                close(m[i][j], expected, 1e-4);
            }
        }
    }

    #[test]
    fn linear_sdr_runtime_respects_disable_env() {
        std::env::set_var("FOCALDESK_LINEAR_SDR", "0");
        assert!(!linear_sdr_runtime_enabled());
        std::env::remove_var("FOCALDESK_LINEAR_SDR");
        assert!(linear_sdr_runtime_enabled());
    }

    #[test]
    fn wp_color_management_respects_disable_env() {
        std::env::remove_var("FOCALDESK_WP_COLOR");
        assert!(wp_color_management_enabled());

        std::env::set_var("FOCALDESK_WP_COLOR", "0");
        assert!(!wp_color_management_enabled());

        std::env::remove_var("FOCALDESK_WP_COLOR");
    }
}
