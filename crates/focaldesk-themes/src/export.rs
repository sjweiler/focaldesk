use crate::builtins::builtin_theme;
use crate::theme::{BuiltInThemeId, FlowTheme, FlowThemeId, UiDensity};
use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

pub fn builtin_theme_css(id: BuiltInThemeId) -> String {
    let theme = builtin_theme(id);
    theme_to_css(&theme)
}

pub fn write_builtin_theme_css(output_dir: impl AsRef<Path>) -> anyhow::Result<Vec<PathBuf>> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let mut written = Vec::new();
    for id in [
        BuiltInThemeId::Eagle,
        BuiltInThemeId::Moonbase,
        BuiltInThemeId::Classic,
    ] {
        let file_name = format!("{}.css", theme_file_stem(id));
        let path = output_dir.join(file_name);
        let css = builtin_theme_css(id);
        fs::write(&path, css).with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }

    Ok(written)
}

fn theme_to_css(theme: &FlowTheme) -> String {
    let id = match &theme.id {
        FlowThemeId::BuiltIn(id) => theme_file_stem(*id).to_string(),
        FlowThemeId::Custom(name) => slugify(name),
    };

    let mut css = String::new();
    css.push_str("/* Generated from focaldesk-themes built-in theme data. */\n");
    css.push_str(":root {\n");
    css.push_str(&format!("  --fd-theme-id: \"{}\";\n", id));
    css.push_str(&format!(
        "  --fd-theme-name: \"{}\";\n",
        escape_css_string(&theme.name)
    ));
    css.push_str(&format!(
        "  --fd-background-color: {};\n",
        rgba(theme.background.color)
    ));
    css.push_str(&format!(
        "  --fd-wallpaper-path: \"{}\";\n",
        escape_css_string(theme.wallpaper.path.as_deref().unwrap_or(""))
    ));
    css.push_str(&format!(
        "  --fd-wallpaper-tint: {};\n",
        rgba(theme.wallpaper.tint_color)
    ));
    css.push_str(&format!("  --fd-wallpaper-dim: {};\n", theme.wallpaper.dim));
    css.push_str(&format!(
        "  --fd-chrome-bg-color: {};\n",
        rgba(theme.chrome.bg_color)
    ));
    css.push_str(&format!(
        "  --fd-chrome-panel-color: {};\n",
        rgba(theme.chrome.panel_color)
    ));
    css.push_str(&format!(
        "  --fd-chrome-accent-color: {};\n",
        rgba(theme.chrome.accent_color)
    ));
    css.push_str(&format!(
        "  --fd-chrome-trim-color: {};\n",
        rgba(theme.chrome.trim_color)
    ));
    css.push_str(&format!(
        "  --fd-chrome-glass-tint: {};\n",
        rgba(theme.chrome.glass_tint)
    ));
    css.push_str(&format!(
        "  --fd-chrome-corner-radius: {}px;\n",
        theme.chrome.corner_radius
    ));
    css.push_str(&format!(
        "  --fd-chrome-border-width: {}px;\n",
        theme.chrome.border_width
    ));
    css.push_str(&format!(
        "  --fd-chrome-glow-intensity: {};\n",
        theme.chrome.glow_intensity
    ));
    css.push_str(&format!(
        "  --fd-chrome-shadow-intensity: {};\n",
        theme.chrome.shadow_intensity
    ));
    css.push_str(&format!(
        "  --fd-dialog-panel-color: {};\n",
        rgba(theme.dialog.panel_color)
    ));
    css.push_str(&format!(
        "  --fd-dialog-title-color: {};\n",
        rgba(theme.dialog.title_color)
    ));
    css.push_str(&format!(
        "  --fd-dialog-text-color: {};\n",
        rgba(theme.dialog.text_color)
    ));
    css.push_str(&format!(
        "  --fd-dialog-button-color: {};\n",
        rgba(theme.dialog.button_color)
    ));
    css.push_str(&format!(
        "  --fd-dialog-overlay-dim: {};\n",
        rgba(theme.dialog.overlay_dim)
    ));
    css.push_str(&format!("  --fd-text-title: {};\n", rgba(theme.text.title)));
    css.push_str(&format!(
        "  --fd-text-normal: {};\n",
        rgba(theme.text.normal)
    ));
    css.push_str(&format!("  --fd-text-dim: {};\n", rgba(theme.text.dim)));
    css.push_str(&format!(
        "  --fd-text-accent: {};\n",
        rgba(theme.text.accent)
    ));
    css.push_str(&format!(
        "  --fd-text-meta-label: {};\n",
        rgba(theme.text.meta_label)
    ));
    css.push_str(&format!(
        "  --fd-text-meta-value: {};\n",
        rgba(theme.text.meta_value)
    ));
    css.push_str(&format!("  --fd-text-clock: {};\n", rgba(theme.text.clock)));
    css.push_str(&format!(
        "  --fd-icon-inactive: {};\n",
        rgba(theme.icons.inactive)
    ));
    css.push_str(&format!(
        "  --fd-icon-hover: {};\n",
        rgba(theme.icons.hover)
    ));
    css.push_str(&format!(
        "  --fd-icon-active: {};\n",
        rgba(theme.icons.active)
    ));
    css.push_str(&format!(
        "  --fd-icon-disabled: {};\n",
        rgba(theme.icons.disabled)
    ));
    css.push_str(&format!("  --fd-icon-glow: {};\n", rgba(theme.icons.glow)));
    css.push_str(&format!("  --fd-spacing: {}px;\n", theme.spacing));
    css.push_str(&format!(
        "  --fd-density: {};\n",
        density_to_css(theme.density)
    ));
    css.push_str(&format!(
        "  --fd-animation-speed: {};\n",
        theme.animation_speed
    ));
    css.push_str(&format!("  --fd-hover-scale: {};\n", theme.hover_scale));
    css.push_str(&format!("  --fd-press-scale: {};\n", theme.press_scale));
    css.push_str(&format!("  --fd-per-output-ui: {};\n", theme.per_output_ui));
    css.push_str("}\n");
    css
}

fn theme_file_stem(id: BuiltInThemeId) -> &'static str {
    match id {
        BuiltInThemeId::Eagle => "eagle",
        BuiltInThemeId::Moonbase => "moonbase",
        BuiltInThemeId::Classic => "classic",
    }
}

fn density_to_css(density: UiDensity) -> &'static str {
    match density {
        UiDensity::Compact => "compact",
        UiDensity::Normal => "normal",
        UiDensity::Spacious => "spacious",
    }
}

fn rgba(color: [f32; 4]) -> String {
    let r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = color[3].clamp(0.0, 1.0);
    format!("rgba({}, {}, {}, {:.3})", r, g, b, a)
}

fn escape_css_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('\"', "\\\"")
}

fn slugify(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'A'..='Z' => ch.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_expected_variables() {
        let css = builtin_theme_css(BuiltInThemeId::Eagle);
        assert!(css.contains("--fd-theme-id: \"eagle\";"));
        assert!(css.contains("--fd-theme-name: \"Eagle\";"));
        assert!(css.contains("--fd-chrome-accent-color: rgba("));
    }
}
