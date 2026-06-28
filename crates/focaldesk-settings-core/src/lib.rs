// crates/focaldesk-settings-core/src/lib.rs
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub appearance: AppearanceSettings,
    pub displays: DisplaySettings,
    pub input: InputSettings,
    pub apps: AppSettings,
    #[serde(default)]
    pub workspaces: WorkspaceSettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
    #[serde(default)]
    pub power: PowerSettings,
    #[serde(default)]
    pub debug: DebugSettings,
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
    #[serde(default)]
    pub color_profile: DisplayColorProfile,
    #[serde(default)]
    pub icc_profile_path: Option<String>,
    #[serde(default)]
    pub hdr_requested: bool,
    #[serde(default)]
    pub hdr_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisplayColorProfile {
    #[default]
    Auto,
    Srgb,
    DisplayP3,
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
    #[serde(default)]
    pub browser_launch_backend: BrowserLaunchBackend,
    pub file_manager: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLaunchBackend {
    #[default]
    Auto,
    Wayland,
    Xwayland,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    #[serde(default = "default_restore_session")]
    pub restore_session: bool,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            restore_session: default_restore_session(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DebugLogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugSettings {
    #[serde(default)]
    pub log_level: DebugLogLevel,
    #[serde(default)]
    pub show_fps: bool,
    #[serde(default)]
    pub show_damage_regions: bool,
    #[serde(default)]
    pub show_input_events: bool,
    #[serde(default)]
    pub verbose_protocol_logs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySettings {
    #[serde(default = "default_recent_files")]
    pub recent_files: bool,
    #[serde(default = "default_location_services")]
    pub location_services: bool,
    #[serde(default = "default_hide_lock_screen_notifications")]
    pub hide_lock_screen_notifications: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            recent_files: default_recent_files(),
            location_services: default_location_services(),
            hide_lock_screen_notifications: default_hide_lock_screen_notifications(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerSettings {
    #[serde(default = "default_blank_screen_minutes")]
    pub blank_screen_minutes: Option<u32>,
    #[serde(default = "default_suspend_minutes")]
    pub suspend_minutes: Option<u32>,
    #[serde(default)]
    pub power_button_action: PowerButtonAction,
    #[serde(default)]
    pub lid_close_action: LidCloseAction,
    #[serde(default)]
    pub low_battery_action: LowBatteryAction,
    #[serde(default)]
    pub performance_mode: PerformanceMode,
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self {
            blank_screen_minutes: default_blank_screen_minutes(),
            suspend_minutes: default_suspend_minutes(),
            power_button_action: PowerButtonAction::default(),
            lid_close_action: LidCloseAction::default(),
            low_battery_action: LowBatteryAction::default(),
            performance_mode: PerformanceMode::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PowerButtonAction {
    #[default]
    ShowPowerMenu,
    Suspend,
    PowerOff,
    DoNothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LidCloseAction {
    #[default]
    Suspend,
    BlankScreen,
    LockScreen,
    DoNothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LowBatteryAction {
    #[default]
    NotifyOnly,
    Suspend,
    Hibernate,
    PowerOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceMode {
    #[default]
    Balanced,
    Performance,
    PowerSaver,
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
            browser_launch_backend: BrowserLaunchBackend::Auto,
            file_manager: "focaldesk-files".into(),
        },
        workspaces: WorkspaceSettings::default(),
        privacy: PrivacySettings::default(),
        power: PowerSettings::default(),
        debug: DebugSettings::default(),
    }
}

fn default_recent_files() -> bool {
    true
}

fn default_restore_session() -> bool {
    true
}

fn default_location_services() -> bool {
    false
}

fn default_hide_lock_screen_notifications() -> bool {
    true
}

fn default_blank_screen_minutes() -> Option<u32> {
    Some(10)
}

fn default_suspend_minutes() -> Option<u32> {
    Some(15)
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk/settings.json")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_restore_setting_defaults_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value.as_object_mut().unwrap().remove("workspaces");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.workspaces.restore_session);

        let mut settings = settings;
        settings.workspaces.restore_session = false;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(!restored.workspaces.restore_session);
    }
}
