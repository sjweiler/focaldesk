use serde::{Deserialize, Serialize};

/// The RGB primaries associated with a theme color.
///
/// Components are stored as linear-light floating point values. RGB values are
/// deliberately not clamped: Display P3 and Rec.2020 colors may be outside the
/// sRGB gamut, and intermediate renderer values may exceed the nominal 0..=1
/// range.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeColorSpace {
    #[default]
    Srgb,
    DisplayP3,
    Rec2020,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ThemeColor {
    pub space: ThemeColorSpace,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl ThemeColor {
    pub const fn new(space: ThemeColorSpace, r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { space, r, g, b, a }
    }

    pub const fn srgb(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::new(ThemeColorSpace::Srgb, r, g, b, a)
    }

    pub const fn display_p3(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::new(ThemeColorSpace::DisplayP3, r, g, b, a)
    }

    pub const fn rec2020(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::new(ThemeColorSpace::Rec2020, r, g, b, a)
    }

    pub const fn components(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Convert linear-light RGB primaries without clamping the result.
    pub fn converted_to(self, target: ThemeColorSpace) -> Self {
        if self.space == target {
            return self;
        }

        let rgb = match (self.space, target) {
            (ThemeColorSpace::DisplayP3, ThemeColorSpace::Srgb) => multiply_matrix(
                [
                    [1.224_745, -0.224_904, 0.0],
                    [-0.042_058, 1.042_081, 0.0],
                    [-0.019_642, -0.078_655, 1.098_537],
                ],
                [self.r, self.g, self.b],
            ),
            (ThemeColorSpace::Srgb, ThemeColorSpace::DisplayP3) => multiply_matrix(
                [
                    [0.822_593, 0.177_534, 0.0],
                    [0.033_200, 0.966_784, 0.0],
                    [0.017_085, 0.072_396, 0.910_301],
                ],
                [self.r, self.g, self.b],
            ),
            (ThemeColorSpace::Rec2020, ThemeColorSpace::Srgb) => multiply_matrix(
                [
                    [1.660_491, -0.587_641, -0.072_850],
                    [-0.124_550, 1.132_900, -0.008_349],
                    [-0.018_151, -0.100_579, 1.118_730],
                ],
                [self.r, self.g, self.b],
            ),
            (ThemeColorSpace::Srgb, ThemeColorSpace::Rec2020) => multiply_matrix(
                [
                    [0.627_404, 0.329_283, 0.043_314],
                    [0.069_097, 0.919_540, 0.011_362],
                    [0.016_392, 0.088_013, 0.895_595],
                ],
                [self.r, self.g, self.b],
            ),
            (ThemeColorSpace::DisplayP3, ThemeColorSpace::Rec2020) => {
                return self
                    .converted_to(ThemeColorSpace::Srgb)
                    .converted_to(ThemeColorSpace::Rec2020);
            }
            (ThemeColorSpace::Rec2020, ThemeColorSpace::DisplayP3) => {
                return self
                    .converted_to(ThemeColorSpace::Srgb)
                    .converted_to(ThemeColorSpace::DisplayP3);
            }
            _ => unreachable!("identical color spaces returned above"),
        };

        Self::new(target, rgb[0], rgb[1], rgb[2], self.a)
    }

    /// Whether this color can be represented by nominal sRGB without mapping.
    pub fn is_in_srgb_gamut(self) -> bool {
        let color = self.converted_to(ThemeColorSpace::Srgb);
        [color.r, color.g, color.b]
            .into_iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(&component))
    }

    /// A simple bounded fallback for previews on sRGB-only output paths.
    ///
    /// This is intentionally separate from storage and conversion so editor
    /// state never loses a wide-gamut selection.
    pub fn mapped_for_srgb_preview(self) -> Self {
        let color = self.converted_to(ThemeColorSpace::Srgb);
        Self::srgb(
            color.r.clamp(0.0, 1.0),
            color.g.clamp(0.0, 1.0),
            color.b.clamp(0.0, 1.0),
            color.a.clamp(0.0, 1.0),
        )
    }
}

impl From<[f32; 4]> for ThemeColor {
    fn from([r, g, b, a]: [f32; 4]) -> Self {
        Self::srgb(r, g, b, a)
    }
}

impl From<ThemeColor> for [f32; 4] {
    fn from(color: ThemeColor) -> Self {
        color.components()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GradientStop {
    pub position: f32,
    pub color: ThemeColor,
}

/// Controls the working space used between gradient stops. A stop's own color
/// space remains unchanged; conversion only happens while evaluating paint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct GradientInterpolation {
    #[serde(default)]
    pub space: ThemeColorSpace,
    #[serde(default = "default_true")]
    pub premultiplied_alpha: bool,
}

impl Default for GradientInterpolation {
    fn default() -> Self {
        Self {
            space: ThemeColorSpace::Srgb,
            premultiplied_alpha: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ThemePaint {
    Solid {
        color: ThemeColor,
    },
    LinearGradient {
        angle: f32,
        #[serde(default)]
        interpolation: GradientInterpolation,
        stops: Vec<GradientStop>,
    },
    RadialGradient {
        center: (f32, f32),
        radius: f32,
        #[serde(default)]
        interpolation: GradientInterpolation,
        stops: Vec<GradientStop>,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeDynamicRange {
    #[default]
    Sdr,
    Hdr,
}

/// Luminance intent is independent from a paint's gamut and geometry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemePaintIntent {
    pub paint: ThemePaint,
    #[serde(default)]
    pub dynamic_range: ThemeDynamicRange,
    #[serde(default = "default_hdr_luminance_nits")]
    pub hdr_luminance_nits: f32,
}

impl ThemePaintIntent {
    pub const SDR_REFERENCE_WHITE_NITS: f32 = 203.0;
    pub const HDR_LUMINANCE_RANGE: std::ops::RangeInclusive<f32> = 203.0..=1_000.0;

    pub fn new(paint: ThemePaint) -> Self {
        Self {
            paint,
            dynamic_range: ThemeDynamicRange::Sdr,
            hdr_luminance_nits: default_hdr_luminance_nits(),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.hdr_luminance_nits.is_finite() {
            return Err("HDR luminance must be finite");
        }
        if !Self::HDR_LUMINANCE_RANGE.contains(&self.hdr_luminance_nits) {
            return Err("HDR luminance is outside the supported range");
        }
        Ok(())
    }

    /// Create an sRGB, SDR-only preview without changing the stored paint.
    pub fn mapped_for_sdr_preview(&self) -> ThemePaint {
        let scale = if self.dynamic_range == ThemeDynamicRange::Hdr {
            self.hdr_luminance_nits / Self::SDR_REFERENCE_WHITE_NITS
        } else {
            1.0
        };
        self.paint.map_colors(|color| {
            let mut color = color.mapped_for_srgb_preview();
            if scale > 1.0 {
                color.r = tone_map_channel(color.r, scale);
                color.g = tone_map_channel(color.g, scale);
                color.b = tone_map_channel(color.b, scale);
            }
            color
        })
    }

    /// Sample authored paint for the compositor's extended-linear-sRGB path.
    ///
    /// Wide-gamut values remain representable outside the nominal sRGB cube,
    /// and HDR intent is expressed relative to the compositor's SDR white.
    pub fn compositor_sample(&self, position: f32) -> Option<ThemeColor> {
        let mut color = self
            .paint
            .sample_ramp(position)?
            .converted_to(ThemeColorSpace::Srgb);
        if self.dynamic_range == ThemeDynamicRange::Hdr {
            let scale = self.hdr_luminance_nits / Self::SDR_REFERENCE_WHITE_NITS;
            color.r *= scale;
            color.g *= scale;
            color.b *= scale;
        }
        color.a = color.a.clamp(0.0, 1.0);
        Some(color)
    }
}

impl ThemePaint {
    pub fn solid(color: ThemeColor) -> Self {
        Self::Solid { color }
    }

    /// Evaluate the color ramp at `position`. Geometry is applied by the
    /// renderer; this method only samples and interpolates the ordered stops.
    pub fn sample_ramp(&self, position: f32) -> Option<ThemeColor> {
        let (stops, interpolation) = match self {
            Self::Solid { color } => return Some(*color),
            Self::LinearGradient {
                stops,
                interpolation,
                ..
            }
            | Self::RadialGradient {
                stops,
                interpolation,
                ..
            } => (stops, *interpolation),
        };

        let first = stops.first()?;
        let last = stops.last()?;
        if position <= first.position {
            return Some(first.color.converted_to(interpolation.space));
        }
        if position >= last.position {
            return Some(last.color.converted_to(interpolation.space));
        }

        let [left, right] = stops
            .windows(2)
            .find(|pair| position >= pair[0].position && position <= pair[1].position)?
        else {
            unreachable!()
        };
        let width = right.position - left.position;
        let amount = if width.abs() <= f32::EPSILON {
            1.0
        } else {
            ((position - left.position) / width).clamp(0.0, 1.0)
        };
        Some(interpolate(
            left.color.converted_to(interpolation.space),
            right.color.converted_to(interpolation.space),
            amount,
            interpolation.premultiplied_alpha,
        ))
    }

    pub fn map_colors(&self, mut map: impl FnMut(ThemeColor) -> ThemeColor) -> Self {
        match self {
            Self::Solid { color } => Self::solid(map(*color)),
            Self::LinearGradient {
                angle,
                interpolation,
                stops,
            } => Self::LinearGradient {
                angle: *angle,
                interpolation: *interpolation,
                stops: stops
                    .iter()
                    .map(|stop| GradientStop {
                        position: stop.position,
                        color: map(stop.color),
                    })
                    .collect(),
            },
            Self::RadialGradient {
                center,
                radius,
                interpolation,
                stops,
            } => Self::RadialGradient {
                center: *center,
                radius: *radius,
                interpolation: *interpolation,
                stops: stops
                    .iter()
                    .map(|stop| GradientStop {
                        position: stop.position,
                        color: map(stop.color),
                    })
                    .collect(),
            },
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_hdr_luminance_nits() -> f32 {
    1_000.0
}

fn tone_map_channel(channel: f32, scale: f32) -> f32 {
    let channel = channel.max(0.0);
    channel * (1.0 + scale) / (1.0 + channel * scale)
}

fn multiply_matrix(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * value[0] + row[1] * value[1] + row[2] * value[2])
}

fn interpolate(
    left: ThemeColor,
    right: ThemeColor,
    amount: f32,
    premultiplied: bool,
) -> ThemeColor {
    let mix = |a: f32, b: f32| a + (b - a) * amount;
    let alpha = mix(left.a, right.a);
    if !premultiplied {
        return ThemeColor::new(
            left.space,
            mix(left.r, right.r),
            mix(left.g, right.g),
            mix(left.b, right.b),
            alpha,
        );
    }

    if alpha.abs() <= f32::EPSILON {
        return ThemeColor::new(left.space, 0.0, 0.0, 0.0, alpha);
    }
    ThemeColor::new(
        left.space,
        mix(left.r * left.a, right.r * right.a) / alpha,
        mix(left.g * left.a, right.g * right.a) / alpha,
        mix(left.b * left.a, right.b * right.a) / alpha,
        alpha,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f32, right: f32) {
        assert!((left - right).abs() < 0.000_1, "{left} != {right}");
    }

    #[test]
    fn p3_primary_is_detected_outside_srgb_without_mutating_it() {
        let red = ThemeColor::display_p3(1.0, 0.0, 0.0, 1.0);
        assert!(!red.is_in_srgb_gamut());
        assert_eq!(red, ThemeColor::display_p3(1.0, 0.0, 0.0, 1.0));
        assert!(red.converted_to(ThemeColorSpace::Srgb).r > 1.0);
    }

    #[test]
    fn neutral_colors_are_shared_by_both_gamuts() {
        let gray = ThemeColor::display_p3(0.4, 0.4, 0.4, 1.0);
        assert!(gray.is_in_srgb_gamut());
        let converted = gray.converted_to(ThemeColorSpace::Srgb);
        close(converted.r, 0.4);
        close(converted.g, 0.4);
        close(converted.b, 0.4);
    }

    #[test]
    fn rec2020_primary_is_preserved_outside_srgb() {
        let green = ThemeColor::rec2020(0.0, 1.0, 0.0, 1.0);
        assert!(!green.is_in_srgb_gamut());
        let converted = green.converted_to(ThemeColorSpace::Srgb);
        assert!(converted.g > 1.0);
        assert!(converted.r < 0.0);
    }

    #[test]
    fn srgb_preview_mapping_does_not_change_stored_p3_color() {
        let color = ThemeColor::display_p3(1.0, 0.0, 0.5, 1.0);
        let preview = color.mapped_for_srgb_preview();
        assert_eq!(color.space, ThemeColorSpace::DisplayP3);
        assert_eq!(preview.space, ThemeColorSpace::Srgb);
        assert!(preview
            .components()
            .into_iter()
            .all(|value| (0.0..=1.0).contains(&value)));
    }

    #[test]
    fn gradient_uses_declared_interpolation_space() {
        let paint = ThemePaint::LinearGradient {
            angle: 90.0,
            interpolation: GradientInterpolation {
                space: ThemeColorSpace::DisplayP3,
                premultiplied_alpha: false,
            },
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: ThemeColor::srgb(1.0, 0.0, 0.0, 1.0),
                },
                GradientStop {
                    position: 1.0,
                    color: ThemeColor::display_p3(0.0, 0.0, 1.0, 1.0),
                },
            ],
        };
        let middle = paint.sample_ramp(0.5).unwrap();
        assert_eq!(middle.space, ThemeColorSpace::DisplayP3);
        close(middle.a, 1.0);
    }

    #[test]
    fn paint_round_trips_through_toml_with_stop_spaces_intact() {
        let paint = ThemePaint::RadialGradient {
            center: (0.25, 0.75),
            radius: 0.8,
            interpolation: GradientInterpolation::default(),
            stops: vec![GradientStop {
                position: 0.0,
                color: ThemeColor::display_p3(1.0, 0.1, 0.0, 0.9),
            }],
        };
        let encoded = toml::to_string(&paint).unwrap();
        let decoded: ThemePaint = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, paint);
    }

    #[test]
    fn hdr_intent_validates_supported_luminance() {
        let mut intent =
            ThemePaintIntent::new(ThemePaint::solid(ThemeColor::srgb(0.5, 0.25, 0.1, 1.0)));
        assert_eq!(intent.validate(), Ok(()));
        intent.hdr_luminance_nits = 202.0;
        assert!(intent.validate().is_err());
        intent.hdr_luminance_nits = 1_001.0;
        assert!(intent.validate().is_err());
        intent.hdr_luminance_nits = f32::NAN;
        assert!(intent.validate().is_err());
    }

    #[test]
    fn hdr_sdr_preview_maps_all_stops_without_mutating_intent() {
        let source = ThemePaint::LinearGradient {
            angle: 90.0,
            interpolation: GradientInterpolation::default(),
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: ThemeColor::display_p3(0.2, 0.1, 0.05, 0.5),
                },
                GradientStop {
                    position: 1.0,
                    color: ThemeColor::srgb(0.8, 0.4, 0.2, 1.0),
                },
            ],
        };
        let intent = ThemePaintIntent {
            paint: source.clone(),
            dynamic_range: ThemeDynamicRange::Hdr,
            hdr_luminance_nits: 1_000.0,
        };
        let preview = intent.mapped_for_sdr_preview();
        let ThemePaint::LinearGradient { stops, .. } = preview else {
            panic!("expected linear preview");
        };
        assert!(stops
            .iter()
            .all(|stop| stop.color.space == ThemeColorSpace::Srgb));
        assert!(stops
            .iter()
            .all(|stop| [stop.color.r, stop.color.g, stop.color.b]
                .into_iter()
                .all(|component| (0.0..=1.0).contains(&component))));
        assert_eq!(intent.paint, source);
        assert_eq!(intent.hdr_luminance_nits, 1_000.0);
    }

    #[test]
    fn sdr_preview_is_identity_for_in_gamut_srgb_color() {
        let color = ThemeColor::srgb(0.25, 0.5, 0.75, 0.8);
        let intent = ThemePaintIntent::new(ThemePaint::solid(color));
        assert_eq!(intent.mapped_for_sdr_preview(), ThemePaint::solid(color));
    }

    #[test]
    fn compositor_sample_preserves_extended_gamut_and_hdr_headroom() {
        let intent = ThemePaintIntent {
            paint: ThemePaint::solid(ThemeColor::display_p3(1.0, 0.0, 0.0, 1.0)),
            dynamic_range: ThemeDynamicRange::Hdr,
            hdr_luminance_nits: 1_000.0,
        };
        let sample = intent.compositor_sample(0.5).unwrap();
        assert_eq!(sample.space, ThemeColorSpace::Srgb);
        assert!(sample.r > 1.0);
        assert!(sample.g < 0.0);
    }
}
