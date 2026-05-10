use std::path::{Path, PathBuf};
use anyhow::{anyhow, Result, Context};
use crate::{FlowTheme, FlowThemeId};
use crate::builtins::builtin_theme;



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
            resolved: Self::resolve_theme(&id),
            active: ActiveTheme::BuiltIn(id),
        }
    }


    pub fn resolve_theme(id: &FlowThemeId) -> FlowTheme {
        match id {
            FlowThemeId::BuiltIn(builtin_id) => builtin_theme(*builtin_id),
            FlowThemeId::Custom(_name) => FlowTheme::default(),
        }
    }
    
    pub fn active_theme(&self) -> &FlowTheme {
        &self.resolved
    }

    pub fn active(&self) -> &ActiveTheme {
        &self.active
    }

    pub fn set_builtin(&mut self, id: FlowThemeId) {
        self.resolved = Self::resolve_theme(&id);
        self.active = ActiveTheme::BuiltIn(id);
    }

    pub fn set_custom(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let theme = load_custom_theme(&path)?;
        self.active = ActiveTheme::Custom(path);
        self.resolved = theme;
        Ok(())
    }
}
