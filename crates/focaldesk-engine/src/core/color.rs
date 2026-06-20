//! Color descriptions and reference transfer functions used by the compositor.

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::{Cacheable, SurfaceData};

/// Pixel color primaries attached to a client surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorPrimaries {
    #[default]
    Srgb,
}

/// Electrical-to-optical transfer function attached to a client surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferFunction {
    #[default]
    Srgb,
    Linear,
}

/// Color interpretation of a surface buffer.
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
        std::env::var("FOCALDESK_FORCE_LINEAR_SURFACES").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

pub fn effective_transfer(states: &SurfaceData, force_linear: bool) -> TransferFunction {
    if force_linear {
        return TransferFunction::Linear;
    }
    states
        .cached_state
        .get::<SurfaceColorState>()
        .current()
        .effective_description()
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
    fn linear_sdr_runtime_respects_disable_env() {
        std::env::set_var("FOCALDESK_LINEAR_SDR", "0");
        assert!(!linear_sdr_runtime_enabled());
        std::env::remove_var("FOCALDESK_LINEAR_SDR");
        assert!(linear_sdr_runtime_enabled());
    }
}
