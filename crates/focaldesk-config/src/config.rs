use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FocalDeskConfig {
    pub appearance: AppearanceConfig,
    pub displays: DisplaysConfig,
    pub shell: ShellConfig,
    pub panel: PanelConfig,
    pub dock: DockConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    pub theme: String,
    pub glow_strength: f64,
    pub font_scale: f64,
    pub output_focus_glow: bool,
    pub shader_chrome: bool,
}

impl FocalDeskConfig {
    pub fn load() -> anyhow::Result<Self> {
        Ok(load_config())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        save_config(self)
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "Eagle".into(),
            glow_strength: 0.75,
            font_scale: 1.0,
            output_focus_glow: true,
            shader_chrome: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ShellStyle {
    Floating,
    #[default]
    Attached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    pub style: ShellStyle,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            style: ShellStyle::Attached,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanelPosition {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub position: PanelPosition,
    pub corner_radius: f64,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            position: PanelPosition::Top,
            corner_radius: 16.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DockPosition {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DockSize {
    Compact,
    #[default]
    Normal,
    Expanded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    pub position: DockPosition,
    pub corner_radius: f64,
    pub size: DockSize,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            position: DockPosition::Left,
            corner_radius: 24.0,
            size: DockSize::Normal,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("config.toml")
}

pub fn load_config() -> FocalDeskConfig {
    let path = config_path();

    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => FocalDeskConfig::default(),
    }
}

pub fn save_config(config: &FocalDeskConfig) -> Result<()> {
    let path = config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let text = toml::to_string_pretty(config)?;
    fs::write(path, text)?;

    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DisplaysConfig {
    pub topbar_on_all_outputs: bool,
    pub sidebar_on_all_outputs: bool,
    pub remember_focused_output: bool,
}

impl Default for DisplaysConfig {
    fn default() -> Self {
        Self {
            topbar_on_all_outputs: true,
            sidebar_on_all_outputs: true,
            remember_focused_output: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_partial_config_gets_shell_defaults() {
        let config: FocalDeskConfig = toml::from_str(
            r#"
            [appearance]
            theme = "Classic"
            glow_strength = 0.5
            font_scale = 1.0
            output_focus_glow = true
            shader_chrome = true
            "#,
        )
        .expect("parse partial configuration");

        assert_eq!(config.shell.style, ShellStyle::Attached);
        assert_eq!(config.panel.position, PanelPosition::Top);
        assert_eq!(config.dock.position, DockPosition::Left);
        assert_eq!(config.dock.size, DockSize::Normal);
    }

    #[test]
    fn shell_configuration_uses_lowercase_toml_values() {
        let config: FocalDeskConfig = toml::from_str(
            r#"
            [shell]
            style = "attached"

            [panel]
            position = "bottom"
            corner_radius = 18

            [dock]
            position = "right"
            corner_radius = 20
            size = "expanded"
            "#,
        )
        .expect("parse shell configuration");

        assert_eq!(config.shell.style, ShellStyle::Attached);
        assert_eq!(config.panel.position, PanelPosition::Bottom);
        assert_eq!(config.dock.position, DockPosition::Right);
        assert_eq!(config.dock.size, DockSize::Expanded);
    }
}
