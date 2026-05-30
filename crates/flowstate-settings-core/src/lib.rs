// crates/flowstate-settings-core/src/lib.rs
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub appearance: AppearanceSettings,
    pub displays: DisplaySettings,
    pub input: InputSettings,
    pub apps: AppSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    pub theme: String,
    pub accent_color: [f32; 4],
    pub sidebar_width: i32,
    pub topbar_height: i32,
    pub icon_size: i32,
    pub animations: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySettings {
    pub outputs: Vec<OutputConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub connector: String,
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    pub scale: f32,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSettings {
    pub pointer_speed: f32,
    pub natural_scroll: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub terminal: String,
    pub browser: String,
    pub file_manager: String,
}

pub fn default_settings() -> Settings {
    Settings {
        appearance: AppearanceSettings {
            theme: "space1999".into(),
            accent_color: [0.1, 0.7, 1.0, 1.0],
            sidebar_width: 64,
            topbar_height: 56,
            icon_size: 32,
            animations: true,
        },
        displays: DisplaySettings { outputs: vec![] },
        input: InputSettings {
            pointer_speed: 1.0,
            natural_scroll: false,
        },
        apps: AppSettings {
            terminal: "alacritty".into(),
            browser: "google-chrome".into(),
            file_manager: "flowstate-files".into(),
        },
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowstate/settings.json")
}

pub fn load_settings() -> Settings {
    let path = settings_path();

    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|_| default_settings()),
        Err(_) => default_settings(),
    }
}

pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let path = settings_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(settings)?;
    fs::write(path, json)
}
