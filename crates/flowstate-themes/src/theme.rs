use serde::{Deserialize, Serialize};
use crate::builtins::{eagle_theme, moonbase_theme, classic_theme, builtin_theme};



#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BuiltInThemeId {
    Eagle,
    Moonbase,
    Classic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FlowThemeId {
    BuiltIn(BuiltInThemeId),
    Custom(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UiDensity {
    Compact,
    Normal,
    Spacious,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowTheme {
    pub id: FlowThemeId,
    pub name: String,

    pub background: BackgroundTheme,
    pub wallpaper: WallpaperTheme,
    pub chrome: ChromeTheme,
    pub dialog: DialogTheme,
    pub text: TextTheme,
    pub icons: IconTheme,

    pub spacing: i32,
    pub density: UiDensity,
    pub animation_speed: f32,
    pub hover_scale: f32,
    pub press_scale: f32,
    pub per_output_ui: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BackgroundTheme {
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallpaperTheme {
    pub path: Option<String>,
    pub tint_color: [f32; 4],
    pub dim: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChromeTheme {
    pub bg_color: [f32; 4],
    pub panel_color: [f32; 4],
    pub accent_color: [f32; 4],
    pub trim_color: [f32; 4],
    pub glass_tint: [f32; 4],
    pub corner_radius: f32,
    pub border_width: f32,
    pub glow_intensity: f32,
    pub shadow_intensity: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DialogTheme {
    pub panel_color: [f32; 4],
    pub title_color: [f32; 4],
    pub text_color: [f32; 4],
    pub button_color: [f32; 4],
    pub overlay_dim: [f32; 4],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TextTheme {
    pub title: [f32; 4],
    pub normal: [f32; 4],
    pub dim: [f32; 4],
    pub accent: [f32; 4],
    pub meta_label: [f32; 4],
    pub meta_value: [f32; 4],  
    pub clock: [f32; 4],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IconTheme {
    pub inactive: [f32; 4],
    pub hover: [f32; 4],
    pub active: [f32; 4],
    pub disabled: [f32; 4],
    pub glow: [f32; 4],
}

impl Default for FlowTheme {
    fn default() -> Self {
        crate::builtins::eagle_theme()
    }
}

pub fn theme_by_name(name: &str) -> FlowTheme {
    match name {
        "Eagle" => builtin_theme(BuiltInThemeId::Eagle),
        "Moonbase" => builtin_theme(BuiltInThemeId::Moonbase),
        "Classic" => builtin_theme(BuiltInThemeId::Classic),
        _ => builtin_theme(BuiltInThemeId::Eagle),
    }
}

impl Default for FlowThemeId {
    fn default() -> Self {
        Self::BuiltIn(BuiltInThemeId::Eagle)
    }
}
