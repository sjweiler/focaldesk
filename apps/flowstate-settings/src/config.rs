use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusShellConfig {
    pub appearance: AppearanceConfig,
    pub displays: DisplaysConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    pub theme: String,
    pub glow_strength: f64,
    pub font_scale: f64,
    pub output_focus_glow: bool,
    pub shader_chrome: bool,
}

impl Default for FocusShellConfig {
    fn default() -> Self {
        Self {
            appearance: AppearanceConfig {
                theme: "Eagle".into(),
                glow_strength: 0.75,
                font_scale: 1.0,
                output_focus_glow: true,
                shader_chrome: true,
            },
            displays: DisplaysConfig::default(),
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowstate")
        .join("config.toml")
}

pub fn load_config() -> FocusShellConfig {
    let path = config_path();

    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => FocusShellConfig::default(),
    }
}

pub fn save_config(config: &FocusShellConfig) -> Result<()> {
    let path = config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let text = toml::to_string_pretty(config)?;
    fs::write(path, text)?;

    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
