use crate::{ThemeColor, ThemeColorSpace, ThemeDynamicRange};
use anyhow::bail;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionState {
    Normal,
    Hover,
    Pressed,
    Selected,
    Focused,
    Urgent,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SurfaceStyle {
    pub normal: ThemeColor,
    #[serde(default)]
    pub hover: Option<ThemeColor>,
    #[serde(default)]
    pub pressed: Option<ThemeColor>,
    #[serde(default)]
    pub selected: Option<ThemeColor>,
    #[serde(default)]
    pub focused: Option<ThemeColor>,
    #[serde(default)]
    pub urgent: Option<ThemeColor>,
    #[serde(default)]
    pub disabled: Option<ThemeColor>,
}

impl SurfaceStyle {
    pub fn resolve(&self, state: InteractionState) -> ThemeColor {
        match state {
            InteractionState::Normal => None,
            InteractionState::Hover => self.hover,
            InteractionState::Pressed => self.pressed,
            InteractionState::Selected => self.selected,
            InteractionState::Focused => self.focused,
            InteractionState::Urgent => self.urgent,
            InteractionState::Disabled => self.disabled,
        }
        .unwrap_or(self.normal)
    }

    pub fn override_for_mut(&mut self, state: InteractionState) -> Option<&mut Option<ThemeColor>> {
        match state {
            InteractionState::Normal => None,
            InteractionState::Hover => Some(&mut self.hover),
            InteractionState::Pressed => Some(&mut self.pressed),
            InteractionState::Selected => Some(&mut self.selected),
            InteractionState::Focused => Some(&mut self.focused),
            InteractionState::Urgent => Some(&mut self.urgent),
            InteractionState::Disabled => Some(&mut self.disabled),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeSurfaces {
    pub bar: SurfaceStyle,
    pub dock: SurfaceStyle,
    pub button: SurfaceStyle,
    pub active_button: SurfaceStyle,
    pub popup: SurfaceStyle,
    pub window_frame: SurfaceStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeEdges {
    pub border_color: ThemeColor,
    pub border_width: f32,
    pub inner_highlight: ThemeColor,
    pub shadow: f32,
    pub glow: f32,
    pub radius: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeTypography {
    pub primary: ThemeColor,
    pub secondary: ThemeColor,
    pub font_weight: u16,
    pub size: f32,
    pub letter_spacing: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeLayout {
    pub bar_height: f32,
    pub dock_width: f32,
    pub padding: f32,
    pub gap: f32,
    pub icon_size: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GamutMapping {
    Clip,
    #[default]
    Perceptual,
    PreserveHue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeColorBehavior {
    pub gamut: ThemeColorSpace,
    pub dynamic_range: ThemeDynamicRange,
    pub sdr_white_nits: f32,
    pub luminance_cap_nits: f32,
    pub gamut_mapping: GamutMapping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WallpaperProcessing {
    pub blur: f32,
    pub saturation: f32,
    pub automatic_accent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticTheme {
    pub surfaces: ThemeSurfaces,
    pub edges: ThemeEdges,
    pub typography: ThemeTypography,
    pub layout: ThemeLayout,
    pub color_behavior: ThemeColorBehavior,
    pub wallpaper: WallpaperProcessing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContrastIssue {
    pub surface: &'static str,
    pub state: InteractionState,
    pub ratio: f32,
    pub required_ratio: f32,
}

impl Default for SurfaceStyle {
    fn default() -> Self {
        Self {
            normal: ThemeColor::srgb(0.10, 0.12, 0.16, 1.0),
            hover: None,
            pressed: None,
            selected: None,
            focused: None,
            urgent: Some(ThemeColor::srgb(0.85, 0.08, 0.04, 1.0)),
            disabled: Some(ThemeColor::srgb(0.08, 0.09, 0.11, 0.55)),
        }
    }
}

impl Default for SemanticTheme {
    fn default() -> Self {
        let base = SurfaceStyle::default();
        let mut active = base.clone();
        active.normal = ThemeColor::srgb(0.05, 0.42, 0.95, 1.0);
        active.hover = Some(ThemeColor::srgb(0.08, 0.55, 1.0, 1.0));
        Self {
            surfaces: ThemeSurfaces {
                bar: base.clone(),
                dock: base.clone(),
                button: base.clone(),
                active_button: active,
                popup: base.clone(),
                window_frame: base,
            },
            edges: ThemeEdges {
                border_color: ThemeColor::srgb(0.35, 0.45, 0.60, 0.8),
                border_width: 1.0,
                inner_highlight: ThemeColor::srgb(1.0, 1.0, 1.0, 0.12),
                shadow: 0.4,
                glow: 0.15,
                radius: 10.0,
            },
            typography: ThemeTypography {
                primary: ThemeColor::srgb(0.92, 0.95, 1.0, 1.0),
                secondary: ThemeColor::srgb(0.55, 0.62, 0.72, 1.0),
                font_weight: 500,
                size: 14.0,
                letter_spacing: 0.0,
            },
            layout: ThemeLayout {
                bar_height: 36.0,
                dock_width: 64.0,
                padding: 12.0,
                gap: 8.0,
                icon_size: 24.0,
            },
            color_behavior: ThemeColorBehavior {
                gamut: ThemeColorSpace::Srgb,
                dynamic_range: ThemeDynamicRange::Sdr,
                sdr_white_nits: 203.0,
                luminance_cap_nits: 1_000.0,
                gamut_mapping: GamutMapping::Perceptual,
            },
            wallpaper: WallpaperProcessing {
                blur: 0.0,
                saturation: 1.0,
                automatic_accent: false,
            },
        }
    }
}

impl SemanticTheme {
    /// Audit primary text against every semantic surface/state combination.
    pub fn contrast_issues(&self) -> Vec<ContrastIssue> {
        let surfaces = [
            ("bar", &self.surfaces.bar),
            ("dock", &self.surfaces.dock),
            ("button", &self.surfaces.button),
            ("active button", &self.surfaces.active_button),
            ("popup", &self.surfaces.popup),
            ("window frame", &self.surfaces.window_frame),
        ];
        let states = [
            InteractionState::Normal,
            InteractionState::Hover,
            InteractionState::Pressed,
            InteractionState::Selected,
            InteractionState::Focused,
            InteractionState::Urgent,
            InteractionState::Disabled,
        ];
        let mut issues = Vec::new();
        for (surface, style) in surfaces {
            for state in states {
                let required_ratio = if state == InteractionState::Disabled {
                    3.0
                } else {
                    4.5
                };
                let ratio = contrast_ratio(style.resolve(state), self.typography.primary);
                if ratio < required_ratio {
                    issues.push(ContrastIssue {
                        surface,
                        state,
                        ratio,
                        required_ratio,
                    });
                }
            }
        }
        issues
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let valid_color = |color: ThemeColor| {
            color.components().into_iter().all(f32::is_finite) && (0.0..=1.0).contains(&color.a)
        };
        let styles = [
            &self.surfaces.bar,
            &self.surfaces.dock,
            &self.surfaces.button,
            &self.surfaces.active_button,
            &self.surfaces.popup,
            &self.surfaces.window_frame,
        ];
        if styles.iter().any(|style| {
            !valid_color(style.normal)
                || [
                    style.hover,
                    style.pressed,
                    style.selected,
                    style.focused,
                    style.urgent,
                    style.disabled,
                ]
                .into_iter()
                .flatten()
                .any(|color| !valid_color(color))
        }) || !valid_color(self.edges.border_color)
            || !valid_color(self.edges.inner_highlight)
            || !valid_color(self.typography.primary)
            || !valid_color(self.typography.secondary)
        {
            bail!("semantic theme color is invalid");
        }
        let finite = [
            self.edges.border_width,
            self.edges.shadow,
            self.edges.glow,
            self.edges.radius,
            self.typography.size,
            self.typography.letter_spacing,
            self.layout.bar_height,
            self.layout.dock_width,
            self.layout.padding,
            self.layout.gap,
            self.layout.icon_size,
            self.color_behavior.sdr_white_nits,
            self.color_behavior.luminance_cap_nits,
            self.wallpaper.blur,
            self.wallpaper.saturation,
        ]
        .into_iter()
        .all(f32::is_finite);
        if !finite {
            bail!("semantic theme values must be finite");
        }
        if !(0.0..=16.0).contains(&self.edges.border_width)
            || !(0.0..=64.0).contains(&self.edges.radius)
            || !(0.0..=1.0).contains(&self.edges.shadow)
            || !(0.0..=1.0).contains(&self.edges.glow)
            || !(8.0..=72.0).contains(&self.typography.size)
            || !(100..=900).contains(&self.typography.font_weight)
            || !(80.0..=400.0).contains(&self.color_behavior.sdr_white_nits)
            || self.color_behavior.luminance_cap_nits < self.color_behavior.sdr_white_nits
            || !(0.0..=64.0).contains(&self.wallpaper.blur)
            || !(0.0..=2.0).contains(&self.wallpaper.saturation)
        {
            bail!("semantic theme value is outside editor limits");
        }
        Ok(())
    }
}

fn contrast_ratio(left: ThemeColor, right: ThemeColor) -> f32 {
    fn luminance(color: ThemeColor) -> f32 {
        let color = color.converted_to(ThemeColorSpace::Srgb);
        0.2126 * color.r.max(0.0) + 0.7152 * color.g.max(0.0) + 0.0722 * color.b.max(0.0)
    }
    let left = luminance(left);
    let right = luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn states_inherit_normal_until_overridden() {
        let style = SurfaceStyle::default();
        assert_eq!(style.resolve(InteractionState::Hover), style.normal);
        assert_ne!(style.resolve(InteractionState::Urgent), style.normal);
    }
    #[test]
    fn defaults_validate() {
        SemanticTheme::default().validate().unwrap();
    }
    #[test]
    fn semantic_overrides_round_trip_without_expanding_inherited_states() {
        let mut theme = SemanticTheme::default();
        theme.surfaces.button.hover = Some(ThemeColor::display_p3(0.8, 0.2, 0.1, 1.0));
        let encoded = toml::to_string(&theme).unwrap();
        let decoded: SemanticTheme = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, theme);
        assert!(decoded.surfaces.button.pressed.is_none());
    }

    #[test]
    fn contrast_audit_identifies_unreadable_states() {
        let mut theme = SemanticTheme::default();
        theme.surfaces.button.normal = theme.typography.primary;
        assert!(theme
            .contrast_issues()
            .iter()
            .any(|issue| { issue.surface == "button" && issue.state == InteractionState::Normal }));
    }
}
