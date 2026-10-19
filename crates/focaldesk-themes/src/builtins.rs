use crate::theme::*;
use crate::ThemeWallpaperFit;

/// A single conservative fallback used only when the system document or a
/// selected package cannot load. User-facing themes are ThemeDocuments.
pub fn builtin_theme(_id: BuiltInThemeId) -> FlowTheme {
    default_theme()
}

pub fn default_theme() -> FlowTheme {
    FlowTheme {
        id: FlowThemeId::BuiltIn(BuiltInThemeId::Eagle),
        name: "Default compatibility".to_string(),
        background: BackgroundTheme {
            color: [0.08, 0.09, 0.10, 1.0],
        },
        wallpaper: WallpaperTheme {
            path: None,
            fit: ThemeWallpaperFit::Fill,
            tint_color: [0.05, 0.15, 0.30, 0.25],
            dim: 0.35,
            blur: 0.0,
            saturation: 1.0,
        },
        chrome: ChromeTheme {
            bg_color: [0.08, 0.09, 0.10, 1.0],
            panel_color: [0.15, 0.16, 0.18, 1.0],
            accent_color: [0.2, 0.6, 1.0, 1.0],
            trim_color: [0.25, 0.45, 0.65, 1.0],
            glass_tint: [0.05, 0.15, 0.30, 0.35],
            corner_radius: 4.0,
            border_width: 1.0,
            glow_intensity: 0.1,
            shadow_intensity: 0.4,
        },
        dialog: DialogTheme {
            panel_color: [0.15, 0.16, 0.18, 1.0],
            title_color: [0.2, 0.6, 1.0, 1.0],
            text_color: [0.9, 0.9, 0.9, 1.0],
            button_color: [0.12, 0.13, 0.15, 1.0],
            overlay_dim: [0.0, 0.0, 0.0, 0.45],
        },
        text: TextTheme {
            title: [0.2, 0.6, 1.0, 1.0],
            normal: [0.9, 0.9, 0.9, 1.0],
            dim: [0.55, 0.58, 0.62, 1.0],
            accent: [0.2, 0.6, 1.0, 1.0],
            meta_label: [0.62, 0.70, 0.80, 1.0],
            meta_value: [1.0, 0.72, 0.18, 1.0],
            clock: [0.65, 0.95, 1.0, 1.0],
        },
        icons: IconTheme {
            inactive: [0.70, 0.75, 0.82, 0.85],
            hover: [0.35, 0.75, 1.0, 1.0],
            active: [0.2, 0.6, 1.0, 1.0],
            disabled: [0.30, 0.32, 0.36, 0.55],
            glow: [0.2, 0.6, 1.0, 0.45],
        },
        semantic: None,
        editor_paint: None,
        semantic_colors_linear: false,
        spacing: 8,
        density: UiDensity::Normal,
        animation_speed: 1.0,
        hover_scale: 1.03,
        press_scale: 0.97,
        per_output_ui: true,
    }
}
