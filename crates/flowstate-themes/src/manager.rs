use std::path::{Path, PathBuf};
use anyhow::{anyhow, Result, Context};
use crate::{FlowTheme, FlowThemeId};

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
}

impl ThemeManager {
    pub fn new(id: FlowThemeId) -> Self {
        Self {
            active: ActiveTheme::BuiltIn(id),
            resolved: FlowTheme::default(),
        }
    }

    pub fn active_theme(&self) -> &FlowTheme {
        &self.resolved
    }

    pub fn active(&self) -> &ActiveTheme {
        &self.active
    }

    pub fn set_builtin(&mut self, id: FlowThemeId) {
        self.active = ActiveTheme::BuiltIn(id);
        self.resolved = FlowTheme::default();
    }

    pub fn set_custom(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let theme = load_custom_theme(&path)?;
        self.active = ActiveTheme::Custom(path);
        self.resolved = theme;
        Ok(())
    }
}
