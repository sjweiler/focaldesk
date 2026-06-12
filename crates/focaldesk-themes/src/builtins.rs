use crate::theme::*;

pub fn builtin_theme(id: BuiltInThemeId) -> FlowTheme {
    match id {
        BuiltInThemeId::Eagle => eagle_theme(),
        BuiltInThemeId::Moonbase => moonbase_theme(),
        BuiltInThemeId::Classic => classic_theme(),
    }
}

pub fn eagle_theme() -> FlowTheme {
    FlowTheme {
        id: FlowThemeId::BuiltIn(BuiltInThemeId::Eagle),
        name: "Eagle".to_string(),

        background: BackgroundTheme {
            color: [0.08, 0.09, 0.10, 1.0],
        },

        wallpaper: WallpaperTheme {
            path: Some("assets/wallpaper/focaldesk_wallpaper.png".to_string()),
            tint_color: [0.05, 0.15, 0.30, 0.25],
            dim: 0.35,
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
            meta_value: [1.00, 0.72, 0.18, 1.0],
            clock: [0.65, 0.95, 1.0, 1.0],
        },

        icons: IconTheme {
            inactive: [0.70, 0.75, 0.82, 0.85],
            hover: [0.35, 0.75, 1.0, 1.0],
            active: [0.2, 0.6, 1.0, 1.0],
            disabled: [0.30, 0.32, 0.36, 0.55],
            glow: [0.2, 0.6, 1.0, 0.45],
        },

        spacing: 6,
        density: UiDensity::Compact,
        animation_speed: 1.5,
        hover_scale: 1.03,
        press_scale: 0.97,
        per_output_ui: true,
    }
}

pub fn moonbase_theme() -> FlowTheme {
    FlowTheme {
        id: FlowThemeId::BuiltIn(BuiltInThemeId::Moonbase),
        name: "Moonbase".to_string(),

        background: BackgroundTheme {
            color: [0.18, 0.19, 0.19, 1.0],
        },

        wallpaper: WallpaperTheme {
            path: Some("assets/wallpaper/focaldesk_wallpaper.png".to_string()),
            tint_color: [0.85, 0.90, 1.00, 0.15],
            dim: 0.20,
        },

        chrome: ChromeTheme {
            bg_color: [0.16, 0.17, 0.17, 1.0],
            panel_color: [0.18, 0.19, 0.20, 1.0],
            accent_color: [0.42, 0.68, 0.86, 1.0],
            trim_color: [0.62, 0.68, 0.72, 1.0],
            glass_tint: [0.75, 0.82, 0.88, 0.18],
            corner_radius: 10.0,
            border_width: 1.0,
            glow_intensity: 0.08,
            shadow_intensity: 0.35,
        },

        dialog: DialogTheme {
            panel_color: [1.0, 1.0, 1.0, 1.0],
            title_color: [0.2, 0.4, 0.8, 1.0],
            text_color: [0.1, 0.1, 0.1, 1.0],
            button_color: [0.88, 0.90, 0.95, 1.0],
            overlay_dim: [0.0, 0.0, 0.0, 0.30],
        },

        text: TextTheme {
            title: [0.88, 0.94, 1.00, 1.0], //[0.2, 0.4, 0.8, 1.0],
            normal: [0.1, 0.1, 0.1, 1.0],
            dim: [0.45, 0.48, 0.52, 1.0],
            accent: [0.2, 0.4, 0.8, 1.0],
            meta_label: [0.78, 0.84, 0.88, 1.0], //[0.45, 0.50, 0.58, 1.0],
            meta_value: [1.00, 0.82, 0.35, 1.0], //[0.18, 0.38, 0.82, 1.0],
            clock: [0.88, 0.94, 1.00, 1.0],
        },

        icons: IconTheme {
            inactive: [0.82, 0.90, 0.96, 0.95],
            hover: [1.00, 1.00, 1.00, 1.00],
            active: [0.55, 0.85, 1.00, 1.00],
            disabled: [0.38, 0.42, 0.45, 0.55],
            glow: [0.65, 0.85, 1.00, 0.35],
        },

        spacing: 10,
        density: UiDensity::Normal,
        animation_speed: 1.0,
        hover_scale: 1.05,
        press_scale: 0.96,
        per_output_ui: true,
    }
}

pub fn classic_theme() -> FlowTheme {
    FlowTheme {
        id: FlowThemeId::BuiltIn(BuiltInThemeId::Classic),
        name: "Classic".to_string(),

        background: BackgroundTheme {
            color: [0.02, 0.02, 0.02, 1.0],
        },

        wallpaper: WallpaperTheme {
            path: Some("assets/wallpaper/focaldesk_wallpaper.png".to_string()),
            tint_color: [1.0, 0.5, 0.1, 0.25],
            dim: 0.45,
        },

        chrome: ChromeTheme {
            bg_color: [0.02, 0.02, 0.02, 1.0],
            panel_color: [0.10, 0.05, 0.00, 1.0],
            accent_color: [1.0, 0.5, 0.0, 1.0],
            trim_color: [0.45, 0.22, 0.04, 1.0],
            glass_tint: [1.0, 0.45, 0.08, 0.30],
            corner_radius: 6.0,
            border_width: 1.0,
            glow_intensity: 0.25,
            shadow_intensity: 0.6,
        },

        dialog: DialogTheme {
            panel_color: [0.10, 0.05, 0.00, 1.0],
            title_color: [1.0, 0.5, 0.0, 1.0],
            text_color: [1.0, 0.6, 0.2, 1.0],
            button_color: [0.16, 0.08, 0.02, 1.0],
            overlay_dim: [0.0, 0.0, 0.0, 0.55],
        },

        text: TextTheme {
            title: [1.0, 0.72, 0.32, 1.0], //  [1.0, 0.5, 0.0, 1.0],
            normal: [1.0, 0.6, 0.2, 1.0],
            dim: [0.65, 0.38, 0.18, 1.0],
            accent: [1.0, 0.5, 0.0, 1.0],
            meta_label: [0.82, 0.58, 0.30, 1.0], // [0.72, 0.42, 0.18, 1.0],
            meta_value: [1.0, 0.82, 0.42, 1.0],  // [1.00, 0.68, 0.20, 1.0],
            clock: [1.00, 0.72, 0.22, 1.0],
        },

        icons: IconTheme {
            inactive: [0.85, 0.45, 0.18, 0.85],
            hover: [1.0, 0.65, 0.20, 1.0],
            active: [1.0, 0.65, 0.20, 1.0],
            disabled: [0.35, 0.22, 0.12, 0.55],
            glow: [1.0, 0.5, 0.0, 0.55],
        },

        spacing: 12,
        density: UiDensity::Spacious,
        animation_speed: 0.8,
        hover_scale: 1.08,
        press_scale: 0.95,
        per_output_ui: false,
    }
}
