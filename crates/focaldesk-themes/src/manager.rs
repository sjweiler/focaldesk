use crate::builtins::builtin_theme;
use crate::{FlowTheme, FlowThemeId, ThemeDocument};
use std::path::{Path, PathBuf};

pub fn load_custom_theme(path: &Path) -> anyhow::Result<FlowTheme> {
    let text = std::fs::read_to_string(path)?;

    let theme: FlowTheme = toml::from_str(&text)?;

    Ok(theme)
}

#[derive(Debug, Clone)]
pub enum ActiveTheme {
    BuiltIn(FlowThemeId),
    Custom(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ThemeManager {
    active: ActiveTheme,
    resolved: FlowTheme,
    editor_applied: Option<FlowTheme>,
    editor_preview: Option<FlowTheme>,
    editor_revision: u64,
}

impl ThemeManager {
    pub fn new(id: FlowThemeId) -> Self {
        Self {
            resolved: Self::resolve_theme(&id),
            active: ActiveTheme::BuiltIn(id),
            editor_applied: None,
            editor_preview: None,
            editor_revision: 0,
        }
    }

    pub fn resolve_theme(id: &FlowThemeId) -> FlowTheme {
        match id {
            FlowThemeId::BuiltIn(builtin_id) => builtin_theme(*builtin_id),
            FlowThemeId::Custom(_name) => FlowTheme::default(),
        }
    }

    pub fn active_theme(&self) -> &FlowTheme {
        self.editor_preview
            .as_ref()
            .or(self.editor_applied.as_ref())
            .unwrap_or(&self.resolved)
    }

    pub fn active(&self) -> &ActiveTheme {
        &self.active
    }

    pub fn set_builtin(&mut self, id: FlowThemeId) {
        self.resolved = Self::resolve_theme(&id);
        self.active = ActiveTheme::BuiltIn(id);
        self.editor_applied = None;
        self.editor_preview = None;
    }

    pub fn set_custom(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let theme = load_custom_theme(&path)?;
        self.active = ActiveTheme::Custom(path);
        self.resolved = theme;
        self.editor_applied = None;
        self.editor_preview = None;
        Ok(())
    }

    pub fn preview_editor_document(&mut self, document: &ThemeDocument) -> anyhow::Result<()> {
        document.validate()?;
        self.editor_preview = Some(project_editor_document(
            self.editor_applied.as_ref().unwrap_or(&self.resolved),
            document,
        )?);
        Ok(())
    }

    pub fn apply_editor_document(&mut self, document: &ThemeDocument) -> anyhow::Result<u64> {
        document.validate()?;
        self.editor_applied = Some(project_editor_document(&self.resolved, document)?);
        self.editor_preview = None;
        self.editor_revision = self.editor_revision.saturating_add(1);
        Ok(self.editor_revision)
    }

    pub fn revert_editor_preview(&mut self) {
        self.editor_preview = None;
    }

    pub fn editor_preview_active(&self) -> bool {
        self.editor_preview.is_some()
    }

    pub fn editor_revision(&self) -> u64 {
        self.editor_revision
    }
}

fn project_editor_document(
    base: &FlowTheme,
    document: &ThemeDocument,
) -> anyhow::Result<FlowTheme> {
    let color = document
        .intent
        .compositor_sample(0.5)
        .ok_or_else(|| anyhow::anyhow!("theme paint has no sampleable color"))?
        .components();
    let mut theme = base.clone();
    theme.name = document.name.clone();
    theme.wallpaper.path = document.wallpaper.path.clone();
    theme.wallpaper.fit = document.wallpaper.fit;
    theme.wallpaper.dim = document.wallpaper.dim;
    theme.wallpaper.blur = document.semantic.wallpaper.blur;
    theme.wallpaper.saturation = document.semantic.wallpaper.saturation;
    theme.wallpaper.tint_color = document
        .wallpaper
        .tint
        .map(|tint| {
            let mut components = tint.converted_to(crate::ThemeColorSpace::Srgb).components();
            for channel in &mut components[..3] {
                *channel = if *channel <= 0.003_130_8 {
                    12.92 * *channel
                } else {
                    1.055 * channel.powf(1.0 / 2.4) - 0.055
                };
            }
            components
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);
    theme.chrome.accent_color = color;
    theme.chrome.trim_color = color;
    theme.chrome.glass_tint = color;
    theme.dialog.button_color = color;
    theme.text.accent = color;
    theme.icons.active = color;
    theme.icons.glow = color;
    let components = |color: crate::ThemeColor| {
        let mut components = color
            .converted_to(crate::ThemeColorSpace::Srgb)
            .components();
        for channel in &mut components[..3] {
            *channel = if *channel <= 0.003_130_8 {
                12.92 * *channel
            } else {
                1.055 * channel.powf(1.0 / 2.4) - 0.055
            };
        }
        components
    };
    theme.chrome.panel_color = components(document.semantic.surfaces.bar.normal);
    theme.chrome.bg_color = components(document.semantic.surfaces.dock.normal);
    theme.dialog.button_color = components(document.semantic.surfaces.button.normal);
    theme.chrome.accent_color = components(document.semantic.surfaces.active_button.normal);
    theme.dialog.panel_color = components(document.semantic.surfaces.popup.normal);
    theme.background.color = components(document.semantic.surfaces.window_frame.normal);
    theme.chrome.border_width = document.semantic.edges.border_width;
    theme.chrome.corner_radius = document.semantic.edges.radius;
    theme.chrome.shadow_intensity = document.semantic.edges.shadow;
    theme.chrome.glow_intensity = document.semantic.edges.glow;
    theme.text.normal = components(document.semantic.typography.primary);
    theme.text.dim = components(document.semantic.typography.secondary);
    theme.spacing = document.semantic.layout.gap.round() as i32;
    theme.icons.hover = components(
        document
            .semantic
            .surfaces
            .active_button
            .resolve(crate::InteractionState::Hover),
    );
    theme.icons.disabled = components(
        document
            .semantic
            .surfaces
            .button
            .resolve(crate::InteractionState::Disabled),
    );
    theme.semantic = Some(document.semantic.clone());
    theme.semantic_colors_linear = false;
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        theme::BuiltInThemeId, ThemeColor, ThemeDynamicRange, ThemePaint, ThemePaintIntent,
    };

    fn document(color: ThemeColor) -> ThemeDocument {
        let mut document = ThemeDocument::new(
            "Live",
            ThemePaintIntent {
                paint: ThemePaint::solid(color),
                dynamic_range: ThemeDynamicRange::Sdr,
                hdr_luminance_nits: 1_000.0,
            },
        );
        document.semantic.surfaces.active_button.normal = color;
        document
    }

    #[test]
    fn preview_reverts_to_last_applied_editor_theme() {
        let mut manager = ThemeManager::new(FlowThemeId::BuiltIn(BuiltInThemeId::Eagle));
        let applied = document(ThemeColor::srgb(0.1, 0.2, 0.3, 1.0));
        manager.apply_editor_document(&applied).unwrap();
        let applied_color = manager.active_theme().chrome.accent_color;

        let preview = document(ThemeColor::srgb(0.8, 0.7, 0.6, 1.0));
        manager.preview_editor_document(&preview).unwrap();
        assert_ne!(manager.active_theme().chrome.accent_color, applied_color);
        assert!(manager.editor_preview_active());

        manager.revert_editor_preview();
        assert_eq!(manager.active_theme().chrome.accent_color, applied_color);
        assert!(!manager.editor_preview_active());
    }

    #[test]
    fn apply_advances_runtime_revision() {
        let mut manager = ThemeManager::new(FlowThemeId::BuiltIn(BuiltInThemeId::Eagle));
        let document = document(ThemeColor::srgb(0.1, 0.2, 0.3, 1.0));
        assert_eq!(manager.apply_editor_document(&document).unwrap(), 1);
        assert_eq!(manager.apply_editor_document(&document).unwrap(), 2);
        assert_eq!(manager.editor_revision(), 2);
    }

    #[test]
    fn editor_projection_keeps_full_semantic_theme_for_the_renderer() {
        let mut manager = ThemeManager::new(FlowThemeId::BuiltIn(BuiltInThemeId::Eagle));
        let mut document = document(ThemeColor::srgb(0.1, 0.2, 0.3, 1.0));
        document.semantic.layout.bar_height = 52.0;
        document.semantic.wallpaper.blur = 12.0;
        manager.preview_editor_document(&document).unwrap();
        let semantic = manager.active_theme().semantic.as_ref().unwrap();
        assert_eq!(semantic.layout.bar_height, 52.0);
        assert_eq!(semantic.wallpaper.blur, 12.0);
        assert_eq!(manager.active_theme().wallpaper.blur, 12.0);
    }
}
