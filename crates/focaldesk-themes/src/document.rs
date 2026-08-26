use crate::{SemanticTheme, ThemeColor, ThemePaint, ThemePaintIntent};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const THEME_DOCUMENT_VERSION: u32 = 1;

/// Portable, versioned source document used by the FocalDesk theme editor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeDocument {
    pub format_version: u32,
    pub name: String,
    #[serde(flatten)]
    pub intent: ThemePaintIntent,
    #[serde(default)]
    pub wallpaper: ThemeWallpaper,
    #[serde(default)]
    pub semantic: SemanticTheme,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeWallpaperFit {
    #[default]
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeWallpaper {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub fit: ThemeWallpaperFit,
    #[serde(default)]
    pub tint: Option<ThemeColor>,
    #[serde(default)]
    pub dim: f32,
}

impl Default for ThemeWallpaper {
    fn default() -> Self {
        Self {
            path: None,
            fit: ThemeWallpaperFit::Fill,
            tint: None,
            dim: 0.0,
        }
    }
}

impl ThemeDocument {
    pub fn new(name: impl Into<String>, intent: ThemePaintIntent) -> Self {
        Self {
            format_version: THEME_DOCUMENT_VERSION,
            name: name.into(),
            intent,
            wallpaper: ThemeWallpaper::default(),
            semantic: SemanticTheme::default(),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format_version != THEME_DOCUMENT_VERSION {
            bail!(
                "unsupported theme format version {}; expected {}",
                self.format_version,
                THEME_DOCUMENT_VERSION
            );
        }
        if self.name.trim().is_empty() {
            bail!("theme name cannot be empty");
        }
        self.intent.validate().map_err(anyhow::Error::msg)?;
        validate_paint(&self.intent.paint)?;
        if let Some(path) = &self.wallpaper.path {
            if path.trim().is_empty() || path.contains('\0') {
                bail!("wallpaper path is invalid");
            }
        }
        if let Some(tint) = self.wallpaper.tint {
            validate_color(tint)?;
        }
        if !self.wallpaper.dim.is_finite() || !(0.0..=1.0).contains(&self.wallpaper.dim) {
            bail!("wallpaper dim must be between 0 and 1");
        }
        self.semantic.validate()?;
        Ok(())
    }

    pub fn from_toml(source: &str) -> anyhow::Result<Self> {
        let document: Self = toml::from_str(source).context("parse theme TOML")?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        self.validate()?;
        toml::to_string_pretty(self).context("serialize theme TOML")
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read theme file {}", path.display()))?;
        Self::from_toml(&source)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let source = self.to_toml()?;
        std::fs::write(path, source).with_context(|| format!("write theme file {}", path.display()))
    }
}

fn validate_paint(paint: &ThemePaint) -> anyhow::Result<()> {
    match paint {
        ThemePaint::Solid { color } => validate_color(*color),
        ThemePaint::LinearGradient { angle, stops, .. } => {
            if !angle.is_finite() {
                bail!("linear gradient angle must be finite");
            }
            validate_stops(stops)
        }
        ThemePaint::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => {
            if !center.0.is_finite() || !center.1.is_finite() {
                bail!("radial gradient center must be finite");
            }
            if !radius.is_finite() || *radius <= 0.0 {
                bail!("radial gradient radius must be positive and finite");
            }
            validate_stops(stops)
        }
    }
}

fn validate_stops(stops: &[crate::GradientStop]) -> anyhow::Result<()> {
    if stops.len() < 2 {
        bail!("gradients require at least two stops");
    }
    let mut previous = None;
    for stop in stops {
        if !stop.position.is_finite() || !(0.0..=1.0).contains(&stop.position) {
            bail!("gradient stop positions must be between 0 and 1");
        }
        if previous.is_some_and(|position| stop.position < position) {
            bail!("gradient stops must be sorted by position");
        }
        previous = Some(stop.position);
        validate_color(stop.color)?;
    }
    Ok(())
}

fn validate_color(color: ThemeColor) -> anyhow::Result<()> {
    if !color.components().into_iter().all(f32::is_finite) {
        bail!("color components must be finite");
    }
    if !(0.0..=1.0).contains(&color.a) {
        bail!("color alpha must be between 0 and 1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GradientInterpolation, GradientStop, ThemeColorSpace, ThemeDynamicRange};

    fn gradient_document() -> ThemeDocument {
        ThemeDocument::new(
            "Aurora",
            ThemePaintIntent {
                paint: ThemePaint::LinearGradient {
                    angle: 42.0,
                    interpolation: GradientInterpolation {
                        space: ThemeColorSpace::DisplayP3,
                        premultiplied_alpha: true,
                    },
                    stops: vec![
                        GradientStop {
                            position: 0.0,
                            color: ThemeColor::display_p3(1.0, 0.1, 0.0, 0.8),
                        },
                        GradientStop {
                            position: 1.0,
                            color: ThemeColor::srgb(0.0, 0.1, 0.8, 1.0),
                        },
                    ],
                },
                dynamic_range: ThemeDynamicRange::Hdr,
                hdr_luminance_nits: 800.0,
            },
        )
    }

    #[test]
    fn document_round_trips_all_editor_metadata() {
        let document = gradient_document();
        let encoded = document.to_toml().unwrap();
        let decoded = ThemeDocument::from_toml(&encoded).unwrap();
        assert_eq!(decoded, document);
        assert!(encoded.contains("format_version = 1"));
        assert!(encoded.contains("dynamic_range = \"hdr\""));
    }

    #[test]
    fn document_rejects_unknown_versions_and_invalid_gradients() {
        let mut document = gradient_document();
        document.format_version = 2;
        assert!(document.validate().is_err());

        document.format_version = THEME_DOCUMENT_VERSION;
        let ThemePaint::LinearGradient { stops, .. } = &mut document.intent.paint else {
            unreachable!()
        };
        stops[1].position = -0.1;
        assert!(document.validate().is_err());
    }

    #[test]
    fn malformed_toml_is_reported_without_a_partial_document() {
        let error = ThemeDocument::from_toml("format_version = nope").unwrap_err();
        assert!(error.to_string().contains("parse theme TOML"));
    }
}
