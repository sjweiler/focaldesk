use crate::theme::UiDensity;
use crate::FlowTheme;

const GTK_APP_CSS_BASE: &str = include_str!("../../../assets/themes/gtk-app.css");

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GtkAppThemeOptions {
    pub font_scale: f64,
    pub animations: bool,
    pub high_contrast: bool,
}

impl Default for GtkAppThemeOptions {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            animations: true,
            high_contrast: false,
        }
    }
}

pub fn gtk_app_css(theme: &FlowTheme, options: GtkAppThemeOptions) -> String {
    let background = theme.background.color;
    let surface = theme.chrome.panel_color;
    let text = if options.high_contrast {
        [1.0, 1.0, 1.0, 1.0]
    } else {
        theme.text.normal
    };
    let accent = theme.chrome.accent_color;
    let accent_bright = mix(accent, [1.0, 1.0, 1.0, 1.0], 0.34);
    let border = if options.high_contrast {
        accent_bright
    } else {
        theme.chrome.trim_color
    };
    let (padding, density_radius_adjustment) = match theme.density {
        UiDensity::Compact => (5, -1.0),
        UiDensity::Normal => (8, 0.0),
        UiDensity::Spacious => (11, 2.0),
    };
    let radius = (theme.chrome.corner_radius + density_radius_adjustment).max(3.0);
    let font_scale = options.font_scale.clamp(0.75, 1.5);
    let transition_ms = if options.animations {
        (150.0 / theme.animation_speed.max(0.1)).round() as u32
    } else {
        0
    };
    let border_width = if options.high_contrast {
        theme.chrome.border_width.max(2.0)
    } else {
        theme.chrome.border_width.max(1.0)
    };

    format!(
        "{colors}\n{base}\n\
         window.focaldesk-app {{ font-size: {font_scale:.3}em; }}\n\
         window.focaldesk-app button {{ padding: {padding}px; border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         window.focaldesk-app entry, window.focaldesk-app searchentry, window.focaldesk-app dropdown {{ border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         window.focaldesk-app .launcher-app-tile, window.focaldesk-app .file-grid-tile, window.focaldesk-app .ai-sidebar, window.focaldesk-app .ai-main, window.focaldesk-app .panel-page, window.focaldesk-app .item-card, window.focaldesk-app .composer {{ border-radius: {radius:.1}px; }}\n\
         window.focaldesk-app .ai-root {{ padding: {double_padding}px; }}\n\
         window.focaldesk-app * {{ transition-duration: {transition_ms}ms; }}\n",
        colors = color_definitions(
            background,
            surface,
            text,
            theme.text.dim,
            accent,
            accent_bright,
            border,
            theme.text.meta_value,
        ),
        base = GTK_APP_CSS_BASE,
        double_padding = padding * 2,
    )
}

pub fn gtk_app_prefers_dark(theme: &FlowTheme) -> bool {
    let [red, green, blue, _] = theme.background.color;
    0.2126 * red + 0.7152 * green + 0.0722 * blue < 0.5
}

#[allow(clippy::too_many_arguments)]
fn color_definitions(
    background: [f32; 4],
    surface: [f32; 4],
    text: [f32; 4],
    text_dim: [f32; 4],
    accent: [f32; 4],
    accent_bright: [f32; 4],
    border: [f32; 4],
    amber: [f32; 4],
) -> String {
    let definitions = [
        ("fd_app_bg", background),
        ("fd_app_surface", surface),
        ("fd_app_surface_raised", mix(surface, text, 0.09)),
        ("fd_app_surface_hover", mix(surface, accent, 0.18)),
        ("fd_app_border", border),
        ("fd_app_border_soft", with_alpha(border, 0.58)),
        ("fd_app_accent", accent),
        ("fd_app_accent_bright", accent_bright),
        ("fd_app_accent_muted", mix(background, accent, 0.42)),
        ("fd_app_amber", amber),
        ("fd_app_text", text),
        ("fd_app_text_dim", text_dim),
        ("fd_app_danger", [0.88, 0.42, 0.46, 1.0]),
        ("fd_app_input", mix(background, [0.0, 0.0, 0.0, 1.0], 0.28)),
    ];

    definitions
        .into_iter()
        .map(|(name, color)| format!("@define-color {name} {};", rgba(color)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn mix(left: [f32; 4], right: [f32; 4], amount: f32) -> [f32; 4] {
    let amount = amount.clamp(0.0, 1.0);
    [
        left[0] + (right[0] - left[0]) * amount,
        left[1] + (right[1] - left[1]) * amount,
        left[2] + (right[2] - left[2]) * amount,
        left[3] + (right[3] - left[3]) * amount,
    ]
}

fn with_alpha(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] = alpha;
    color
}

fn rgba(color: [f32; 4]) -> String {
    format!(
        "rgba({:.0}, {:.0}, {:.0}, {:.3})",
        color[0].clamp(0.0, 1.0) * 255.0,
        color[1].clamp(0.0, 1.0) * 255.0,
        color[2].clamp(0.0, 1.0) * 255.0,
        color[3].clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::{gtk_app_css, gtk_app_prefers_dark, GtkAppThemeOptions};
    use crate::theme::theme_by_name;

    #[test]
    fn generated_css_tracks_palette_scale_density_and_motion() {
        let classic = theme_by_name("Classic");
        let css = gtk_app_css(
            &classic,
            GtkAppThemeOptions {
                font_scale: 1.25,
                animations: false,
                high_contrast: true,
            },
        );

        assert!(css.contains("@define-color fd_app_accent rgba(255, 128, 0"));
        assert!(css.contains("font-size: 1.250em"));
        assert!(css.contains("padding: 11px"));
        assert!(css.contains("transition-duration: 0ms"));
        assert!(css.contains("border-width: 2.0px"));
        assert!(css.contains(".shell-surface"));
        assert!(gtk_app_prefers_dark(&classic));
    }
}
