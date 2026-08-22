//! Color descriptions and reference transfer functions used by the compositor.
//!
//! Phase A/B: scene-linear Rec.709 working space, relative gamut mapping.
//! Phase C1: SDR scanout encode uses each output's ICC/EDID description.
//! Phase C2: colord D-Bus hotplug/profile refresh (`core::colord`).

use focaldesk_settings_core::DisplayColorProfile;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::{Cacheable, SurfaceData};

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

/// True when `a` covers a wider gamut than `b` (named primaries ordering only).
pub fn primaries_wider_than(a: ColorPrimaries, b: ColorPrimaries) -> bool {
    fn rank(p: ColorPrimaries) -> u8 {
        match p {
            ColorPrimaries::Srgb => 0,
            ColorPrimaries::DisplayP3 => 1,
            ColorPrimaries::Bt2020 => 2,
            ColorPrimaries::Custom(_) => 3,
        }
    }
    rank(a) > rank(b)
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
    /// Piecewise sRGB extended above 1.0 (Wayland `ext_srgb`, Chromium `SRGB_HDR`).
    SrgbHdr,
}

/// Shader decode mode sent as `u_decode_tf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TransferDecodeMode {
    SrgbPiecewise = 0,
    LinearPassThrough = 1,
    Gamma22 = 2,
    St2084Pq = 3,
    SrgbHdrExtended = 4,
}

impl TransferFunction {
    pub fn decode_mode(self) -> TransferDecodeMode {
        match self {
            Self::Srgb | Self::Bt1886 => TransferDecodeMode::SrgbPiecewise,
            Self::Gamma22 => TransferDecodeMode::Gamma22,
            Self::Linear => TransferDecodeMode::LinearPassThrough,
            Self::St2084Pq => TransferDecodeMode::St2084Pq,
            Self::SrgbHdr => TransferDecodeMode::SrgbHdrExtended,
        }
    }

    /// Electrical encoding applied when writing the KMS framebuffer.
    pub fn encode_mode(self) -> TransferDecodeMode {
        match self {
            Self::Srgb | Self::Bt1886 | Self::SrgbHdr => TransferDecodeMode::SrgbPiecewise,
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
    /// `create_windows_scrgb` stimulus: R=G=B=1.0 is 80 cd/m², not paper white.
    /// Parametric ExtLinear with a 203-nit reference is a different encoding.
    pub windows_scrgb_stimulus: bool,
}

/// Diffuse/graphics white used when placing SDR desktop content in an HDR PQ signal.
///
/// ITU-R BT.2408 maps SDR reference white to 203 cd/m² in the PQ container. Keep
/// this separate from the 80-nit reference carried by an SDR output profile: that
/// profile describes the source scene, not the luminance of white on an HDR output.
pub const HDR_REFERENCE_WHITE_NITS: f32 = 203.0;

/// HDR reference white, bounded for displays that report a lower usable peak.
pub fn hdr_reference_white_nits(max_luminance_nits: f32) -> f32 {
    HDR_REFERENCE_WHITE_NITS.min(max_luminance_nits.max(1.0))
}

/// DisplayHDR 400-class ceiling for authored HDR10 highlight energy.
///
/// ASUS VG32VQR-class VA panels advertise HDR10 and about 450 nits usable
/// peak, but they have no local dimming. Inventing 800–1000 nit highlights
/// only clips or trips the monitor's global tone map. Type-1 EDID often
/// quantizes that peak to 409 nits; encode and KMS metadata use 450 so the
/// panel's tone map matches the PQ we actually send.
pub const HDR_CONSERVATIVE_PEAK_NITS: f32 = 450.0;

/// Usable HDR10 peak for encode, preferred client volume, and KMS metadata.
///
/// DisplayHDR 400 EDID commonly reports ~409 nits. These ASUS VA panels still
/// reach about 450, so values at or below that ceiling are raised to 450.
/// Brighter EDID peaks stay capped at [`HDR_CONSERVATIVE_PEAK_NITS`].
pub fn hdr_conservative_peak_nits(max_luminance_nits: f32) -> f32 {
    let _ = max_luminance_nits.max(1.0);
    HDR_CONSERVATIVE_PEAK_NITS
}

/// CTA-861 Type-1 has no SDR-white field. MaxFALL carries BT.2408 graphics
/// white so the monitor's global tone map treats desktop average as paper
/// white instead of a full-frame 450-nit peak.
pub fn hdr10_kms_max_luminance_nits() -> u16 {
    HDR_CONSERVATIVE_PEAK_NITS.round() as u16
}

pub fn hdr10_kms_max_cll_nits() -> u16 {
    hdr10_kms_max_luminance_nits()
}

pub fn hdr10_kms_max_fall_nits() -> u16 {
    HDR_REFERENCE_WHITE_NITS.round() as u16
}

/// PQ-encode luminance map: keep graphics white and most in-range HDR.
///
/// Rolling from paper white with a 10,000 nit source crushed 400 nit content
/// on a 450 nit panel. Starting at 80% of peak keeps those values, and still
/// compresses extremes so 8-bit PQ clouds do not posterize into magenta.
pub fn tone_map_hdr_nits(value: f32, source_peak: f32, display_peak: f32, white: f32) -> f32 {
    let display_peak = display_peak.max(1.0);
    let white = white.max(1.0).min(display_peak);
    let knee = white.max(display_peak * 0.8);
    if value <= knee || display_peak <= knee {
        return value.min(display_peak);
    }
    let peak = source_peak.max(knee + 0.0001);
    let range = (display_peak - knee).max(0.0001);
    let denominator = 1.0 - (-(peak - knee) / range).exp();
    let numerator = 1.0 - (-(value - knee) / range).exp();
    knee + range * numerator / denominator.max(0.0001)
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
        windows_scrgb_stimulus: false,
    };

    pub const LINEAR_SRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Linear,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Windows-scRGB (`create_windows_scrgb`): sRGB primaries, extended linear.
    /// Sample 1.0 is 80 cd/m²; assumed graphics white is 2.5375 (203 cd/m²).
    pub const WINDOWS_SCRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Linear,
        reference_white_nits: 203.0,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: true,
    };

    pub const DISPLAY_P3_SRGB: Self = Self {
        primaries: ColorPrimaries::DisplayP3,
        transfer: TransferFunction::Srgb,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Chrome's Wayland HDR raster space: Display P3, extended sRGB, 1.0 = paper white.
    pub const DISPLAY_P3_SRGB_HDR: Self = Self {
        primaries: ColorPrimaries::DisplayP3,
        transfer: TransferFunction::SrgbHdr,
        reference_white_nits: HDR_REFERENCE_WHITE_NITS,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Linear BT.2020 with sample 1.0 as paper white. Chrome tags FP16 windows
    /// as HDR10 PQ but stores linear BT.2020, not ST.2084 and not Display P3.
    pub const BT2020_LINEAR_HDR: Self = Self {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Linear,
        reference_white_nits: HDR_REFERENCE_WHITE_NITS,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// BT.2020 with extended sRGB. Chrome's FP16 HDR window often still has
    /// the sRGB OETF in the samples; linear decode leaves reds orange.
    pub const BT2020_SRGB_HDR: Self = Self {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::SrgbHdr,
        reference_white_nits: HDR_REFERENCE_WHITE_NITS,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Extended linear Rec.709 with sample 1.0 as paper white.
    ///
    /// Chrome's Wayland HDR raster is scRGB-shaped (709 primaries, values
    /// outside 0–1 for P3) even when it tags the window BT.2020/PQ. This is
    /// not Windows-scRGB: 1.0 is 203 nits, not 80.
    pub const SCRGB_LINEAR_HDR: Self = Self {
        primaries: ColorPrimaries::Srgb,
        transfer: TransferFunction::Linear,
        reference_white_nits: HDR_REFERENCE_WHITE_NITS,
        max_luminance_nits: 10_000.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Wide-gamut SDR capture target. The portal shader applies the precise
    /// BT.709 OETF; `Bt1886` is the closest SDR video transfer description in
    /// the compositor's current surface/output model.
    pub const BT2020_SDR: Self = Self {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Bt1886,
        reference_white_nits: 80.0,
        max_luminance_nits: 80.0,
        max_cll_nits: None,
        max_fall_nits: None,
        windows_scrgb_stimulus: false,
    };

    /// Display P3 + PQ. Kept for named P3 HDR10 surfaces; P3-class preferred
    /// descriptions use `DISPLAY_P3_SRGB_HDR` because Chrome's 8-bit window
    /// cannot carry ST.2084. Scanout is still BT.2020/PQ.
    pub fn display_p3_pq_hdr(max_luminance_nits: f32, max_fall_nits: f32) -> Self {
        Self {
            primaries: ColorPrimaries::DisplayP3,
            transfer: TransferFunction::St2084Pq,
            reference_white_nits: hdr_reference_white_nits(max_luminance_nits),
            max_luminance_nits,
            max_cll_nits: Some(max_luminance_nits),
            max_fall_nits: Some(max_fall_nits),
            windows_scrgb_stimulus: false,
        }
    }

    /// HDR preferred description for clients.
    ///
    /// Chrome rasters HDR in Display P3 with extended sRGB (1.0 = paper white).
    /// 8-bit windows cannot carry ST.2084, so P3-class panels advertise that
    /// raster space. PQ-tagging 8-bit P3 made the W nearly disappear (both
    /// reds clip to the same peak) and crushed SDR/HDR stills. Scanout is
    /// still BT.2020/PQ. Named BT.2020 panels keep BT.2020+PQ.
    pub fn hdr_preferred_from_panel(
        panel: Self,
        max_luminance_nits: f32,
        max_fall_nits: f32,
    ) -> Self {
        let peak = hdr_conservative_peak_nits(max_luminance_nits);
        let white = hdr_reference_white_nits(peak);
        let _ = max_fall_nits;
        match panel.primaries {
            ColorPrimaries::Bt2020 => {
                let mut hdr = Self::bt2020_pq_hdr(peak, white);
                hdr.max_cll_nits = Some(peak);
                hdr.max_fall_nits = Some(white);
                hdr
            }
            _ => {
                let mut hdr = Self::DISPLAY_P3_SRGB_HDR;
                hdr.max_luminance_nits = peak;
                hdr.max_cll_nits = Some(peak);
                hdr.max_fall_nits = Some(white);
                hdr
            }
        }
    }

    /// BT.2020 + PQ from EDID Type-1 static metadata (HDR scanout target).
    pub fn bt2020_pq_hdr(max_luminance_nits: f32, max_fall_nits: f32) -> Self {
        Self {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::St2084Pq,
            reference_white_nits: hdr_reference_white_nits(max_luminance_nits),
            max_luminance_nits,
            max_cll_nits: Some(max_luminance_nits),
            max_fall_nits: Some(max_fall_nits),
            windows_scrgb_stimulus: false,
        }
    }

    /// Windows-scRGB stimulus encoding from `create_windows_scrgb` or an
    /// equivalent parametric ExtLinear description.
    ///
    /// This is not ordinary sRGB: sample 1.0 is 80 cd/m², graphics white is
    /// 203 cd/m² (sample 2.5375), values may be negative, and values above 1.0
    /// are HDR headroom. The compositor must keep those samples linear.
    pub fn is_windows_scrgb(self) -> bool {
        self.windows_scrgb_stimulus
    }

    /// Scale decoded linear samples into the scene convention where 1.0 is
    /// reference white.
    ///
    /// Windows-scRGB is unusual: sample value 1.0 means 80 cd/m², while its
    /// assumed reference white is sample value 2.5375 (203 cd/m²). Other
    /// linear descriptions in the current model already use 1.0 as reference
    /// white and therefore need no adjustment.
    pub fn linear_to_scene_scale(self) -> f32 {
        if self.is_windows_scrgb() {
            80.0 / HDR_REFERENCE_WHITE_NITS
        } else {
            1.0
        }
    }
}

/// Pixel packing of a client `wl_buffer`, used to catch image descriptions that
/// do not match the samples actually uploaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientBufferEncoding {
    #[default]
    Unknown,
    Unorm8,
    Unorm10,
    Float16,
}

/// Chrome's 8-bit HDR window is Display P3 with an sRGB-style transfer, even
/// when it copies a PQ tag. PQ-decoding those samples clips both W-test reds
/// to the same peak and wrecks still shading. 10-bit packed RGB can be real
/// HDR10. FP16 tagged PQ is linear Rec.709, not ST.2084.
pub fn sanitize_tagged_client_description(
    description: ColorDescription,
    buffer: ClientBufferEncoding,
) -> ColorDescription {
    match buffer {
        ClientBufferEncoding::Float16 if description.transfer == TransferFunction::St2084Pq => {
            ColorDescription::SCRGB_LINEAR_HDR
        }
        ClientBufferEncoding::Unorm8 | ClientBufferEncoding::Unknown
            if matches!(
                description.transfer,
                TransferFunction::St2084Pq | TransferFunction::SrgbHdr
            ) =>
        {
            ColorDescription::DISPLAY_P3_SRGB_HDR
        }
        _ => description,
    }
}

/// Preferred client descriptions that must stay parametric (no SDR ICC file).
pub fn is_hdr_client_preferred_transfer(transfer: TransferFunction) -> bool {
    matches!(
        transfer,
        TransferFunction::St2084Pq | TransferFunction::SrgbHdr
    )
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
    /// Encoded-source bit depth for decode dither. GTK and Chrome commonly
    /// submit `AB24`; 10-bit/FP16 sources leave this at 0.
    pub src_bits: f32,
}

impl SurfaceColorRenderState {
    pub fn for_description(description: ColorDescription, intent: RenderingIntent) -> Self {
        let client_to_scene =
            gamut_matrix_linear_rgb(description.primaries, scene_working_primaries(), intent);
        Self {
            description,
            intent,
            client_to_scene,
            src_bits: 0.0,
        }
    }

    pub fn with_buffer_encoding(mut self, encoding: ClientBufferEncoding) -> Self {
        // Record the packing independently of the tagged transfer. Ordinary
        // 8-bit sRGB GTK gradients need decode dither too when promoted to
        // HDR10; the render path gates the dither off for SDR outputs.
        self.src_bits = if encoding == ClientBufferEncoding::Unorm8 {
            8.0
        } else {
            0.0
        };
        self
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
        if let (Some(max), Some(_fall)) = (hdr_max_luminance_nits, hdr_max_fall_nits) {
            let peak = hdr_conservative_peak_nits(max);
            return ColorDescription::bt2020_pq_hdr(peak, hdr_reference_white_nits(peak));
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

/// Destination primaries for HDR10 gamut mapping.
///
/// P3-class panels (named P3 or a factory ICC) map into Display P3 D65 so the
/// wide-gamut W stays in-gamut and HDR white stays D65. Swapping only the ICC
/// white without adapting the RGB xy distorts that volume and turns the W
/// orange and HDR clouds magenta. A missing ICC does not crush P3 into sRGB.
/// A named BT.2020 panel is a no-op.
pub fn hdr10_encode_panel_primaries(panel: ColorPrimaries) -> ColorPrimaries {
    match panel {
        ColorPrimaries::Srgb | ColorPrimaries::Bt2020 => ColorPrimaries::Bt2020,
        ColorPrimaries::DisplayP3 | ColorPrimaries::Custom(_) => ColorPrimaries::DisplayP3,
    }
}

/// Scene Rec.709 → panel RGB, panel RGB → BT.2020, and panel luminance coeffs.
pub fn hdr10_pq_encode_transforms(
    panel: ColorPrimaries,
) -> ([[f32; 3]; 3], [[f32; 3]; 3], [f32; 3]) {
    let dest = hdr10_encode_panel_primaries(panel);
    let scene_to_panel =
        gamut_matrix_linear_rgb(scene_working_primaries(), dest, RenderingIntent::Relative);
    let panel_to_bt2020 =
        gamut_matrix_linear_rgb(dest, ColorPrimaries::Bt2020, RenderingIntent::Relative);
    let xyz = primaries_to_rgb_to_xyz(dest.chromaticity());
    let luma = [xyz[1][0], xyz[1][1], xyz[1][2]];
    (scene_to_panel, panel_to_bt2020, luma)
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

/// Enable the FP16 scene-linear compositor unless explicitly disabled.
///
/// Wide-gamut output advertisement depends on this path because the legacy
/// 8-bit sRGB intermediate cannot preserve out-of-sRGB scene values. Keep an
/// opt-out for driver diagnosis without silently disabling color management
/// in normal sessions.
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

/// When true, apply HDR connector properties to the persistent scanout path (C3c).
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

/// NVIDIA live KMS HDR with more than one active output.
pub fn hdr_nvidia_dual_enabled() -> bool {
    matches!(
        std::env::var("FOCALDESK_HDR_NVIDIA_DUAL").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// `FOCALDESK_HDR_OUTPUT` is set to one exact connector for a mixed HDR/SDR test.
pub fn hdr_output_selector_active() -> bool {
    normalized_env_value("FOCALDESK_HDR_OUTPUT").is_some()
}

/// Limit experimental HDR rendering and KMS changes to one connector.
///
/// This is a development safety rail, not a persistent display preference. An
/// unset or empty selector allows every output whose normal HDR preference is
/// enabled; otherwise connector names must match exactly (for example `DP-3`).
pub fn hdr_output_selected(output_name: &str) -> bool {
    let hdr_selector = normalized_env_value("FOCALDESK_HDR_OUTPUT");
    let exclusive_selector = exclusive_hdr_output_selector();
    hdr_output_selected_with_selectors(
        output_name,
        hdr_selector.as_deref(),
        exclusive_selector.as_deref(),
    )
}

fn hdr_output_selected_with_selector(output_name: &str, selector: Option<&str>) -> bool {
    selector
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
        .is_none_or(|selector| selector == output_name)
}

fn hdr_output_selected_with_selectors(
    output_name: &str,
    hdr_selector: Option<&str>,
    exclusive_selector: Option<&str>,
) -> bool {
    let selector = exclusive_selector
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
        .or_else(|| {
            hdr_selector
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
        });
    hdr_output_selected_with_selector(output_name, selector)
}

fn normalized_env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Exact connector selected for the session-start-only exclusive HDR mode.
///
/// The DRM backend validates this selector before removing any other output
/// from the active topology. Keeping the parser here also lets the normal HDR
/// safety filter constrain KMS changes to the same connector.
pub fn exclusive_hdr_output_selector() -> Option<String> {
    let state = focaldesk_settings_core::load_exclusive_hdr_state();
    if state.phase == focaldesk_settings_core::ExclusiveHdrPhase::Disabled {
        return None;
    }
    normalized_env_value("FOCALDESK_EXCLUSIVE_HDR_OUTPUT").or_else(|| {
        state
            .phase
            .selects_output()
            .then_some(state.connector)
            .flatten()
    })
}

/// Whether the DRM loop may attempt live KMS HDR commits.
///
/// A previous exclusive-HDR failure only blocks automatic exclusive retry. It
/// must not prevent Apply Requested HDR10 or a persisted `hdr_requested` flag
/// from applying after logout, suspend, restart, or shutdown.
pub fn hdr_runtime_may_apply_kms(any_output_hdr_requested: bool) -> bool {
    hdr_runtime_may_apply_kms_with_state(any_output_hdr_requested)
}

fn hdr_runtime_may_apply_kms_with_state(any_output_hdr_requested: bool) -> bool {
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
        assert_eq!(hdr.max_luminance_nits, HDR_CONSERVATIVE_PEAK_NITS);
        close(hdr.reference_white_nits, HDR_REFERENCE_WHITE_NITS, 0.0);
        assert_eq!(hdr.max_fall_nits, Some(HDR_REFERENCE_WHITE_NITS));
    }

    #[test]
    fn monitor_gamut_override_does_not_replace_hdr10_scanout_space() {
        for profile in [
            DisplayColorProfile::Auto,
            DisplayColorProfile::Srgb,
            DisplayColorProfile::DisplayP3,
        ] {
            let sdr =
                apply_output_color_profile_override(ColorDescription::DISPLAY_P3_SRGB, profile);
            let hdr = kms_scanout_encode_description(sdr, true, Some(1_000.0), Some(400.0));
            assert_eq!(hdr.primaries, ColorPrimaries::Bt2020);
            assert_eq!(hdr.transfer, TransferFunction::St2084Pq);
        }
    }

    #[test]
    fn display_p3_surface_survives_scene_to_bt2020_hdr_conversion() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }

        let p3_red = [1.0, 0.0, 0.0];
        let p3_to_scene = gamut_matrix_linear_rgb(
            ColorPrimaries::DisplayP3,
            scene_working_primaries(),
            RenderingIntent::Relative,
        );
        let scene_to_bt2020 = gamut_matrix_linear_rgb(
            scene_working_primaries(),
            ColorPrimaries::Bt2020,
            RenderingIntent::Relative,
        );
        let direct_p3_to_bt2020 = gamut_matrix_linear_rgb(
            ColorPrimaries::DisplayP3,
            ColorPrimaries::Bt2020,
            RenderingIntent::Relative,
        );
        let through_scene = transform(scene_to_bt2020, transform(p3_to_scene, p3_red));
        let direct = transform(direct_p3_to_bt2020, p3_red);

        for channel in 0..3 {
            close(through_scene[channel], direct[channel], 1e-4);
        }
        assert!(through_scene[0] > through_scene[1]);
        assert!(through_scene[1] > through_scene[2]);
    }

    #[test]
    fn windows_scrgb_anchors_eighty_nits_and_hdr_reference_white() {
        let scale = ColorDescription::WINDOWS_SCRGB.linear_to_scene_scale();
        assert!(ColorDescription::WINDOWS_SCRGB.is_windows_scrgb());
        assert!(!ColorDescription::LINEAR_SRGB.is_windows_scrgb());
        assert!(!ColorDescription::SRGB.is_windows_scrgb());
        close(1.0 * scale * HDR_REFERENCE_WHITE_NITS, 80.0, 1e-5);
        close(2.5375 * scale, 1.0, 1e-5);
        close(
            ColorDescription::LINEAR_SRGB.linear_to_scene_scale(),
            1.0,
            0.0,
        );
    }

    #[test]
    fn pq_tagged_chrome_buffers_decode_as_scrgb_linear_hdr() {
        let claimed = ColorDescription::bt2020_pq_hdr(409.0, 400.0);
        let fp16 = sanitize_tagged_client_description(claimed, ClientBufferEncoding::Float16);
        assert!(!fp16.is_windows_scrgb());
        assert_eq!(fp16, ColorDescription::SCRGB_LINEAR_HDR);
        assert_eq!(fp16.primaries, ColorPrimaries::Srgb);
        assert_eq!(fp16.transfer, TransferFunction::Linear);
        close(fp16.linear_to_scene_scale(), 1.0, 0.0);

        let pq10 = sanitize_tagged_client_description(claimed, ClientBufferEncoding::Unorm10);
        assert_eq!(pq10.transfer, TransferFunction::St2084Pq);
        assert_eq!(pq10.primaries, ColorPrimaries::Bt2020);

        let unorm8 = sanitize_tagged_client_description(claimed, ClientBufferEncoding::Unorm8);
        assert_eq!(unorm8.transfer, TransferFunction::SrgbHdr);
        assert_eq!(unorm8.primaries, ColorPrimaries::DisplayP3);
        assert!(!unorm8.is_windows_scrgb());
    }

    #[test]
    fn eight_bit_clients_enable_decode_dither() {
        let pq = ColorDescription::bt2020_pq_hdr(450.0, 400.0);
        let eight = SurfaceColorRenderState::for_description(pq, RenderingIntent::Relative)
            .with_buffer_encoding(ClientBufferEncoding::Unorm8);
        assert_eq!(eight.src_bits, 8.0);
        let ten = SurfaceColorRenderState::for_description(pq, RenderingIntent::Relative)
            .with_buffer_encoding(ClientBufferEncoding::Unorm10);
        assert_eq!(ten.src_bits, 0.0);

        let gtk = SurfaceColorRenderState::for_description(
            ColorDescription::SRGB,
            RenderingIntent::Perceptual,
        )
        .with_buffer_encoding(ClientBufferEncoding::Unorm8);
        assert_eq!(gtk.src_bits, 8.0);

        let chrome = sanitize_tagged_client_description(pq, ClientBufferEncoding::Unorm8);
        let chrome_eight =
            SurfaceColorRenderState::for_description(chrome, RenderingIntent::Relative)
                .with_buffer_encoding(ClientBufferEncoding::Unorm8);
        assert_eq!(chrome.transfer, TransferFunction::SrgbHdr);
        assert_eq!(chrome_eight.src_bits, 8.0);

        let tagged = SurfaceColorRenderState::for_description(
            ColorDescription::DISPLAY_P3_SRGB_HDR,
            RenderingIntent::Relative,
        )
        .with_buffer_encoding(ClientBufferEncoding::Unorm8);
        assert_eq!(tagged.src_bits, 8.0);
    }

    #[test]
    fn bt2020_linear_keeps_srgb_red_from_turning_orange() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }

        let srgb_red_in_bt2020 = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::Srgb,
                ColorPrimaries::Bt2020,
                RenderingIntent::Relative,
            ),
            [1.0, 0.0, 0.0],
        );
        let through_tagged = transform(
            SurfaceColorRenderState::for_description(
                ColorDescription::BT2020_LINEAR_HDR,
                RenderingIntent::Relative,
            )
            .client_to_scene,
            srgb_red_in_bt2020,
        );
        let through_p3 = transform(
            SurfaceColorRenderState::for_description(
                ColorDescription::DISPLAY_P3_SRGB_HDR,
                RenderingIntent::Relative,
            )
            .client_to_scene,
            srgb_red_in_bt2020,
        );
        let back_to_2020 = gamut_matrix_linear_rgb(
            scene_working_primaries(),
            ColorPrimaries::Bt2020,
            RenderingIntent::Relative,
        );
        let tagged_out = transform(back_to_2020, through_tagged);
        let p3_out = transform(back_to_2020, through_p3);

        close(tagged_out[0], srgb_red_in_bt2020[0], 1e-4);
        close(tagged_out[1], srgb_red_in_bt2020[1], 1e-4);
        assert!(p3_out[1] / p3_out[0] > tagged_out[1] / tagged_out[0] + 0.05);
    }

    #[test]
    fn scrgb_linear_hdr_encodes_srgb_red_as_bt2020_red_not_bt2020_primary() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }

        let scene = SurfaceColorRenderState::for_description(
            ColorDescription::SCRGB_LINEAR_HDR,
            RenderingIntent::Relative,
        )
        .client_to_scene;
        let to_2020 = gamut_matrix_linear_rgb(
            scene_working_primaries(),
            ColorPrimaries::Bt2020,
            RenderingIntent::Relative,
        );
        let out = transform(to_2020, transform(scene, [1.0, 0.0, 0.0]));
        let srgb_red_in_bt2020 = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::Srgb,
                ColorPrimaries::Bt2020,
                RenderingIntent::Relative,
            ),
            [1.0, 0.0, 0.0],
        );
        close(out[0], srgb_red_in_bt2020[0], 1e-4);
        close(out[1], srgb_red_in_bt2020[1], 1e-4);
        assert!(out[0] < 0.75);
        assert!(out[1] > 0.04);
    }

    #[test]
    fn srgb_encoded_bt2020_red_needs_srgb_decode_to_stay_red() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }

        let linear_2020 = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::Srgb,
                ColorPrimaries::Bt2020,
                RenderingIntent::Relative,
            ),
            [1.0, 0.0, 0.0],
        );
        let encoded = [
            linear_to_srgb(linear_2020[0]),
            linear_to_srgb(linear_2020[1]),
            linear_to_srgb(linear_2020[2]),
        ];
        let decoded = [
            srgb_to_linear(encoded[0]),
            srgb_to_linear(encoded[1]),
            srgb_to_linear(encoded[2]),
        ];
        assert!(encoded[1] / encoded[0] > 0.25);
        assert!(decoded[1] / decoded[0] < 0.15);
        close(decoded[0], linear_2020[0], 1e-5);
        close(decoded[1], linear_2020[1], 1e-5);
    }

    #[test]
    fn parametric_ext_linear_is_not_windows_scrgb_stimulus() {
        let description = ColorDescription {
            primaries: ColorPrimaries::Srgb,
            transfer: TransferFunction::Linear,
            reference_white_nits: 203.0,
            max_luminance_nits: 10_000.0,
            max_cll_nits: None,
            max_fall_nits: None,
            windows_scrgb_stimulus: false,
        };
        assert!(!description.is_windows_scrgb());
        close(description.linear_to_scene_scale(), 1.0, 0.0);
    }

    #[test]
    fn hdr_preferred_uses_p3_srgb_hdr_on_p3_class_panels() {
        let panel = ColorDescription {
            primaries: ColorPrimaries::Custom(PrimariesChromaticity {
                r: [0.6780116, 0.31299728],
                g: [0.2830347, 0.6479949],
                b: [0.14802192, 0.067950554],
                w: [0.34569982, 0.35850027],
            }),
            transfer: TransferFunction::Srgb,
            reference_white_nits: 80.0,
            max_luminance_nits: 80.0,
            max_cll_nits: None,
            max_fall_nits: None,
            windows_scrgb_stimulus: false,
        };
        let preferred = ColorDescription::hdr_preferred_from_panel(panel, 409.0, 400.0);
        assert_eq!(preferred.transfer, TransferFunction::SrgbHdr);
        assert_eq!(preferred.primaries, ColorPrimaries::DisplayP3);
        close(
            preferred.reference_white_nits,
            HDR_REFERENCE_WHITE_NITS,
            0.0,
        );
        close(
            preferred.max_luminance_nits,
            HDR_CONSERVATIVE_PEAK_NITS,
            0.0,
        );
        assert_eq!(preferred.max_cll_nits, Some(HDR_CONSERVATIVE_PEAK_NITS));
        assert_eq!(preferred.max_fall_nits, Some(HDR_REFERENCE_WHITE_NITS));
        let rec2020_panel = ColorDescription {
            primaries: ColorPrimaries::Bt2020,
            ..panel
        };
        let rec2020_preferred =
            ColorDescription::hdr_preferred_from_panel(rec2020_panel, 1_000.0, 400.0);
        assert_eq!(rec2020_preferred.primaries, ColorPrimaries::Bt2020);
        assert_eq!(rec2020_preferred.transfer, TransferFunction::St2084Pq);
        close(
            rec2020_preferred.max_luminance_nits,
            HDR_CONSERVATIVE_PEAK_NITS,
            0.0,
        );
        close(
            rec2020_preferred.reference_white_nits,
            HDR_REFERENCE_WHITE_NITS,
            0.0,
        );
        assert_eq!(
            hdr10_encode_panel_primaries(panel.primaries),
            ColorPrimaries::DisplayP3
        );
        assert_eq!(
            hdr10_encode_panel_primaries(ColorPrimaries::Srgb),
            ColorPrimaries::Bt2020
        );
        assert_eq!(
            hdr10_encode_panel_primaries(ColorPrimaries::Bt2020),
            ColorPrimaries::Bt2020
        );
    }

    #[test]
    fn hdr10_gamut_map_keeps_p3_red_and_pulls_rec2020_green_into_panel() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }
        fn compress(rgb: [f32; 3], luma: [f32; 3]) -> [f32; 3] {
            let minc = rgb[0].min(rgb[1]).min(rgb[2]);
            if minc >= 0.0 {
                return rgb;
            }
            let y = (luma[0] * rgb[0] + luma[1] * rgb[1] + luma[2] * rgb[2]).max(0.0);
            let dest = [y, y, y];
            let mut t = 0.0f32;
            for i in 0..3 {
                let denom = rgb[i] - dest[i];
                if rgb[i] < 0.0 && denom.abs() > 1e-6 {
                    t = t.max(rgb[i] / denom);
                }
            }
            t = t.clamp(0.0, 1.0);
            [
                rgb[0] + t * (dest[0] - rgb[0]),
                rgb[1] + t * (dest[1] - rgb[1]),
                rgb[2] + t * (dest[2] - rgb[2]),
            ]
        }

        let (scene_to_p3, _, p3_luma) = hdr10_pq_encode_transforms(ColorPrimaries::DisplayP3);

        let p3_red_scene = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::DisplayP3,
                scene_working_primaries(),
                RenderingIntent::Relative,
            ),
            [1.0, 0.0, 0.0],
        );
        let p3_red_panel = transform(scene_to_p3, p3_red_scene);
        assert!(p3_red_panel.iter().all(|c| *c >= -1e-4));

        let rec2020_green_scene = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::Bt2020,
                scene_working_primaries(),
                RenderingIntent::Relative,
            ),
            [0.0, 1.0, 0.0],
        );
        let rec2020_green_panel = transform(scene_to_p3, rec2020_green_scene);
        assert!(rec2020_green_panel.iter().any(|c| *c < 0.0));
        let mapped = compress(rec2020_green_panel, p3_luma);
        assert!(mapped.iter().all(|c| *c >= -1e-4));
    }

    #[test]
    fn hdr10_highlights_stay_inside_displayhdr_450_headroom() {
        assert_eq!(
            hdr_conservative_peak_nits(350.0),
            HDR_CONSERVATIVE_PEAK_NITS
        );
        assert_eq!(
            hdr_conservative_peak_nits(409.0),
            HDR_CONSERVATIVE_PEAK_NITS
        );
        assert_eq!(
            hdr_conservative_peak_nits(450.0),
            HDR_CONSERVATIVE_PEAK_NITS
        );
        assert_eq!(
            hdr_conservative_peak_nits(1_000.0),
            HDR_CONSERVATIVE_PEAK_NITS
        );
        assert_eq!(hdr_reference_white_nits(409.0), HDR_REFERENCE_WHITE_NITS);
        assert_eq!(hdr10_kms_max_luminance_nits(), 450);
        assert_eq!(hdr10_kms_max_cll_nits(), 450);
        assert_eq!(hdr10_kms_max_fall_nits(), 203);
    }

    #[test]
    fn hdr10_tone_map_keeps_in_range_highlights() {
        let paper = tone_map_hdr_nits(203.0, 10_000.0, 450.0, 203.0);
        close(paper, 203.0, 0.5);
        let in_range = tone_map_hdr_nits(400.0, 10_000.0, 450.0, 203.0);
        assert!(
            in_range > 380.0,
            "400 nit HDR must stay near 400, got {in_range}"
        );
        assert!(in_range <= 450.0);
        let peak = tone_map_hdr_nits(10_000.0, 10_000.0, 450.0, 203.0);
        close(peak, 450.0, 1.0);
        let over = tone_map_hdr_nits(1_000.0, 10_000.0, 450.0, 203.0);
        assert!(over > 405.0);
        assert!(over <= 450.0);
    }

    #[test]
    fn hdr10_rec2020_blue_channel_may_exceed_luminance_peak() {
        // A 400 nit Rec.2020 primary blue has Y=400 and B=400/0.0593 ≈ 6,745 nits.
        // Scaling B down to a 450 nit panel peak would leave ~27 nits of luminance.
        let y = 400.0f32;
        let b_channel = y / 0.0593;
        assert!(b_channel > 6_000.0);
        let crushed = y * (450.0 / b_channel);
        assert!(crushed < 40.0);
        let mapped = tone_map_hdr_nits(y, 10_000.0, 450.0, 203.0);
        assert!(mapped > 380.0);
        assert!(mapped <= 450.0);
    }

    #[test]
    fn hdr10_d65_white_survives_warm_icc_panel() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }
        let warm = ColorPrimaries::Custom(PrimariesChromaticity {
            r: [0.6780116, 0.31299728],
            g: [0.2830347, 0.6479949],
            b: [0.14802192, 0.067950554],
            w: [0.34569982, 0.35850027],
        });
        assert_eq!(
            hdr10_encode_panel_primaries(warm),
            ColorPrimaries::DisplayP3
        );
        let (scene_to_panel, panel_to_bt2020, _) = hdr10_pq_encode_transforms(warm);
        let bt2020 = transform(panel_to_bt2020, transform(scene_to_panel, [1.0, 1.0, 1.0]));
        close(bt2020[0], 1.0, 0.02);
        close(bt2020[1], 1.0, 0.02);
        close(bt2020[2], 1.0, 0.02);

        let p3_red_scene = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::DisplayP3,
                scene_working_primaries(),
                RenderingIntent::Relative,
            ),
            [1.0, 0.0, 0.0],
        );
        let p3_red_panel = transform(scene_to_panel, p3_red_scene);
        assert!(
            p3_red_panel.iter().all(|c| *c >= -1e-4),
            "P3 red must stay in-gamut for the W test, got {p3_red_panel:?}"
        );
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
        assert!(!hdr_runtime_may_apply_kms_with_state(true));
        std::env::remove_var("FOCALDESK_HDR");

        assert!(hdr_runtime_may_apply_kms_with_state(true));
        assert!(!hdr_runtime_may_apply_kms_with_state(false));
        assert!(!output_hdr_render_active(false, true, true));
        assert!(!output_hdr_render_active(true, false, true));
    }

    #[test]
    fn hdr_output_selector_limits_hdr_to_one_connector() {
        assert!(hdr_output_selected_with_selector("DP-3", None));
        assert!(hdr_output_selected_with_selector("DP-3", Some("")));
        assert!(hdr_output_selected_with_selector("DP-3", Some(" DP-3 ")));
        assert!(!hdr_output_selected_with_selector("DP-4", Some("DP-3")));
    }

    #[test]
    fn exclusive_failed_state_does_not_keep_selecting_a_connector() {
        use focaldesk_settings_core::ExclusiveHdrPhase;
        assert!(!ExclusiveHdrPhase::Failed.selects_output());
        assert!(hdr_output_selected_with_selectors("DP-3", None, None));
        assert!(hdr_output_selected_with_selectors("DP-4", None, None));
    }

    #[test]
    fn exclusive_selector_takes_precedence_over_general_hdr_selector() {
        assert!(!hdr_output_selected_with_selectors(
            "DP-3",
            Some("DP-3"),
            Some("HDMI-A-1")
        ));
        assert!(hdr_output_selected_with_selectors(
            "HDMI-A-1",
            Some("DP-3"),
            Some("HDMI-A-1")
        ));
        assert!(hdr_output_selected_with_selectors(
            "HDMI-A-1",
            Some(""),
            Some(" HDMI-A-1 ")
        ));
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
    fn display_p3_round_trip_requires_unclipped_linear_scene() {
        fn transform(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
            [
                matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
                matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
                matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
            ]
        }

        let p3_red = [1.0, 0.0, 0.0];
        let scene = transform(
            gamut_matrix_linear_rgb(
                ColorPrimaries::DisplayP3,
                scene_working_primaries(),
                RenderingIntent::Relative,
            ),
            p3_red,
        );
        assert!(scene[0] > 1.0 || scene[1] < 0.0 || scene[2] < 0.0);

        let recovered = transform(
            scene_to_output_matrix(ColorDescription::DISPLAY_P3_SRGB, RenderingIntent::Relative),
            scene,
        );
        for channel in 0..3 {
            assert!((recovered[channel] - p3_red[channel]).abs() < 1e-4);
        }

        let clipped_scene = scene.map(|channel| channel.clamp(0.0, 1.0));
        let clipped_result = transform(
            scene_to_output_matrix(ColorDescription::DISPLAY_P3_SRGB, RenderingIntent::Relative),
            clipped_scene,
        );
        assert!(clipped_result
            .into_iter()
            .zip(p3_red)
            .any(|(actual, expected)| (actual - expected).abs() > 0.02));
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
        std::env::remove_var("FOCALDESK_LINEAR_SDR");
        assert!(linear_sdr_runtime_enabled());

        std::env::set_var("FOCALDESK_LINEAR_SDR", "1");
        assert!(linear_sdr_runtime_enabled());

        std::env::set_var("FOCALDESK_LINEAR_SDR", "0");
        assert!(!linear_sdr_runtime_enabled());

        std::env::remove_var("FOCALDESK_LINEAR_SDR");
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
