use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    pub work_area_glass: bool,
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
            theme: "Default".into(),
            glow_strength: 0.75,
            font_scale: 1.0,
            output_focus_glow: true,
            shader_chrome: true,
            work_area_glass: true,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ClockFormat {
    #[default]
    #[serde(rename = "12-hour")]
    TwelveHour,
    #[serde(rename = "24-hour")]
    TwentyFourHour,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub position: PanelPosition,
    pub corner_radius: f64,
    pub clock_format: ClockFormat,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            position: PanelPosition::Top,
            corner_radius: 16.0,
            clock_format: ClockFormat::TwelveHour,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DockVisibility {
    AlwaysVisible,
    #[default]
    IntelligentDodge,
    Autohide,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    pub position: DockPosition,
    pub corner_radius: f64,
    pub size: DockSize,
    pub visibility: DockVisibility,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            position: DockPosition::Left,
            corner_radius: 24.0,
            size: DockSize::Normal,
            visibility: DockVisibility::IntelligentDodge,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("settings.json")
}

fn legacy_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("config.toml")
}

/// Returns the theme explicitly selected in the user's configuration.
///
/// This intentionally differs from [`load_config`]: a missing, unreadable, or
/// partial configuration returns `None` instead of the built-in configuration
/// default, allowing the compositor to use its system-installed default theme.
pub fn configured_theme() -> Option<String> {
    if let Ok(text) = fs::read_to_string(config_path()) {
        if let Some(config) = config_from_settings_json(&text) {
            return Some(config.appearance.theme);
        }
    }
    let text = fs::read_to_string(legacy_config_path()).ok()?;
    configured_theme_from_toml(&text)
}

fn configured_theme_from_toml(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    value
        .get("appearance")?
        .get("theme")?
        .as_str()
        .map(str::to_owned)
}

fn config_from_settings_json(text: &str) -> Option<FocalDeskConfig> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(value.get("desktop_config")?.clone()).ok()
}

pub fn load_config() -> FocalDeskConfig {
    load_config_from_paths(&config_path(), &legacy_config_path())
}

fn load_config_from_paths(settings_path: &Path, legacy_path: &Path) -> FocalDeskConfig {
    if let Ok(text) = fs::read_to_string(settings_path) {
        if let Some(config) = config_from_settings_json(&text) {
            return config;
        }
    }

    let config = fs::read_to_string(legacy_path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();

    // Migration is deliberately best-effort here because configuration loading
    // must remain available on read-only or full filesystems. The explicit
    // migration entry point below reports errors to callers that need them.
    let _ = migrate_legacy_config_at(settings_path, legacy_path);
    config
}

pub fn save_config(config: &FocalDeskConfig) -> Result<()> {
    save_config_at(&config_path(), config)
}

/// Copy a valid legacy `config.toml` into the canonical `settings.json`.
///
/// The source is retained as a recovery copy. Existing canonical desktop
/// configuration always wins, while unrelated top-level settings are
/// preserved. Returns `true` only when a migration was written.
pub fn migrate_legacy_config() -> Result<bool> {
    migrate_legacy_config_at(&config_path(), &legacy_config_path())
}

fn migrate_legacy_config_at(settings_path: &Path, legacy_path: &Path) -> Result<bool> {
    let legacy_text = match fs::read_to_string(legacy_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let config: FocalDeskConfig = toml::from_str(&legacy_text)?;

    let mut root = read_settings_root(settings_path)?;
    if root.contains_key("desktop_config") {
        return Ok(false);
    }
    root.insert("desktop_config".into(), serde_json::to_value(config)?);
    write_settings_root(settings_path, &root)?;
    Ok(true)
}

fn save_config_at(path: &Path, config: &FocalDeskConfig) -> Result<()> {
    let mut root = read_settings_root(path)?;
    root.insert("desktop_config".into(), serde_json::to_value(config)?);
    write_settings_root(path, &root)
}

fn read_settings_root(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text)?;
            let Some(root) = value.as_object().cloned() else {
                bail!("{} must contain a JSON object", path.display());
            };
            Ok(root)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(error.into()),
    }
}

fn write_settings_root(
    path: &Path,
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let text = serde_json::to_string_pretty(root)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    write_result
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

    fn test_dir(name: &str) -> PathBuf {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "focaldesk-config-{name}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn partial_config_without_theme_has_no_explicit_theme() {
        assert_eq!(
            configured_theme_from_toml("[appearance]\nfont_scale = 1.25\n"),
            None
        );
    }

    #[test]
    fn configured_theme_returns_explicit_selection() {
        assert_eq!(
            configured_theme_from_toml("[appearance]\ntheme = \"Classic\"\n"),
            Some("Classic".to_string())
        );
    }

    #[test]
    fn canonical_settings_json_contains_typed_config() {
        let config =
            config_from_settings_json(r#"{"desktop_config":{"appearance":{"theme":"Classic"}}}"#)
                .expect("parse canonical configuration");
        assert_eq!(config.appearance.theme, "Classic");
        assert_eq!(config.dock.visibility, DockVisibility::IntelligentDodge);
    }

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
        assert!(config.appearance.work_area_glass);
        assert_eq!(config.panel.position, PanelPosition::Top);
        assert_eq!(config.panel.clock_format, ClockFormat::TwelveHour);
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
            clock_format = "24-hour"

            [dock]
            position = "right"
            corner_radius = 20
            size = "expanded"
            "#,
        )
        .expect("parse shell configuration");

        assert_eq!(config.shell.style, ShellStyle::Attached);
        assert_eq!(config.panel.position, PanelPosition::Bottom);
        assert_eq!(config.panel.clock_format, ClockFormat::TwentyFourHour);
        assert_eq!(config.dock.position, DockPosition::Right);
        assert_eq!(config.dock.size, DockSize::Expanded);
    }

    #[test]
    fn legacy_config_is_migrated_without_removing_source() {
        let dir = test_dir("migrate");
        fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        let legacy = dir.join("config.toml");
        fs::write(
            &legacy,
            "[appearance]\ntheme = \"Classic\"\nglow_strength = 0.5\nfont_scale = 1.0\n",
        )
        .unwrap();

        let loaded = load_config_from_paths(&settings, &legacy);

        assert_eq!(loaded.appearance.theme, "Classic");
        assert!(legacy.exists());
        let canonical = fs::read_to_string(&settings).unwrap();
        assert_eq!(
            config_from_settings_json(&canonical)
                .unwrap()
                .appearance
                .theme,
            "Classic"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migration_preserves_other_settings_and_never_overwrites_canonical_config() {
        let dir = test_dir("preserve");
        fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        let legacy = dir.join("config.toml");
        fs::write(&settings, r#"{"privacy":{"clipboard_history":false}}"#).unwrap();
        fs::write(&legacy, "[appearance]\ntheme = \"Classic\"\n").unwrap();

        assert!(migrate_legacy_config_at(&settings, &legacy).unwrap());
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(root["privacy"]["clipboard_history"], false);

        fs::write(&legacy, "[appearance]\ntheme = \"Eagle\"\n").unwrap();
        assert!(!migrate_legacy_config_at(&settings, &legacy).unwrap());
        let canonical = fs::read_to_string(&settings).unwrap();
        assert_eq!(
            config_from_settings_json(&canonical)
                .unwrap()
                .appearance
                .theme,
            "Classic"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_canonical_settings_are_not_destroyed_during_migration() {
        let dir = test_dir("malformed");
        fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        let legacy = dir.join("config.toml");
        fs::write(&settings, "not json\n").unwrap();
        fs::write(&legacy, "[appearance]\ntheme = \"Classic\"\n").unwrap();

        assert!(migrate_legacy_config_at(&settings, &legacy).is_err());
        assert_eq!(fs::read_to_string(&settings).unwrap(), "not json\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
