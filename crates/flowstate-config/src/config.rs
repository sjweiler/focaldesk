use anyhow::{Context, Result};
use flowstate_themes::FlowThemeId;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FlowConfig {
    #[serde(default)]
    pub theme: ThemeSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ThemeSection {
    #[serde(default)]
    pub active: Option<FlowThemeId>,
}

impl FlowConfig {
    pub fn load() -> Result<Self> {
        let path = config_file_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {:?}", path))?;
        eprintln!("FLOWSTATE config path = {:?}", path);
        eprintln!("FLOWSTATE raw config:\n{}", raw);
        
        
        toml::from_str(&raw).with_context(|| format!("parse {:?}", path))
    }
}

fn config_file_path() -> PathBuf {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|| PathBuf::from("."))
        });
    xdg.join("flowstate").join("config.toml")
}
