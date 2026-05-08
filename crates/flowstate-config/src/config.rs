#[derive(Debug, Clone, Deserialize)]
pub struct FlowConfig {
    pub apps: AppsConfig,
    pub spawn: SpawnConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppsConfig {
    pub terminal: String,
    pub browser: String,
    pub file_manager: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnConfig {
    pub default_workspace: Option<u32>,
    pub focus_new_windows: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    pub bars: BarsConfig,
    pub theme: ThemeConfig,
}

use once_cell::sync::Lazy;

pub static CONFIG: Lazy<FlowConfig> = Lazy::new(|| {
    load().expect("Failed to load FlowState config")
});
