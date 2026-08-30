// crates/focaldesk-settings-core/src/lib.rs
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};

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
    #[serde(default)]
    pub chrome: ChromeSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChromeSettings {
    #[serde(default)]
    pub sidebar: ChromeRegionSettings,
    #[serde(default)]
    pub topbar: ChromeRegionSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChromeRegionSettings {
    /// Stable numeric element IDs in preferred order. Unlisted built-ins are
    /// appended, so older settings remain forward-compatible.
    #[serde(default)]
    pub order: Vec<u32>,
    #[serde(default)]
    pub hidden: Vec<u32>,
    #[serde(default)]
    pub custom: Vec<ChromeLaunchItemSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChromeLaunchItemSettings {
    pub id: u32,
    pub icon: String,
    pub tooltip: String,
    pub command: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    pub theme: String,
    pub accent_color: [f32; 4],
    pub sidebar_width: i32,
    pub topbar_height: i32,
    pub icon_size: i32,
    pub animations: bool,
    #[serde(default)]
    pub high_contrast: bool,
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
    #[serde(default)]
    pub hdr_appearance: HdrAppearance,
}

/// Per-output creative controls for the final HDR10 encode pass.
///
/// These values never enable HDR or change KMS connector state. They are only
/// consumed after the compositor has independently selected a guarded HDR10
/// output path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HdrAppearance {
    pub reference_white_nits: f32,
    pub peak_nits: f32,
    pub saturation: f32,
    pub midtone_gamma: f32,
}

impl Default for HdrAppearance {
    fn default() -> Self {
        Self {
            reference_white_nits: 203.0,
            peak_nits: 450.0,
            saturation: 1.0,
            midtone_gamma: 1.0,
        }
    }
}

impl HdrAppearance {
    // The current HDR10 metadata contract advertises a 450-nit mastering
    // ceiling. Shader tuning must not create pixels above that promise.
    pub const REFERENCE_WHITE_RANGE: std::ops::RangeInclusive<f32> = 80.0..=450.0;
    pub const PEAK_RANGE: std::ops::RangeInclusive<f32> = 203.0..=450.0;
    pub const SATURATION_RANGE: std::ops::RangeInclusive<f32> = 0.75..=1.25;
    pub const MIDTONE_GAMMA_RANGE: std::ops::RangeInclusive<f32> = 0.70..=1.50;

    pub fn validate(self) -> Result<Self, &'static str> {
        if !self.reference_white_nits.is_finite()
            || !self.peak_nits.is_finite()
            || !self.saturation.is_finite()
            || !self.midtone_gamma.is_finite()
        {
            return Err("HDR appearance values must be finite");
        }
        if !Self::REFERENCE_WHITE_RANGE.contains(&self.reference_white_nits) {
            return Err("HDR reference white is outside the supported range");
        }
        if !Self::PEAK_RANGE.contains(&self.peak_nits) {
            return Err("HDR peak luminance is outside the supported range");
        }
        if self.reference_white_nits > self.peak_nits {
            return Err("HDR reference white cannot exceed peak luminance");
        }
        if !Self::SATURATION_RANGE.contains(&self.saturation) {
            return Err("HDR saturation is outside the supported range");
        }
        if !Self::MIDTONE_GAMMA_RANGE.contains(&self.midtone_gamma) {
            return Err("HDR midtone gamma is outside the supported range");
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisplayColorProfile {
    #[default]
    Auto,
    Srgb,
    DisplayP3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExclusiveHdrPhase {
    #[default]
    Off,
    Disabled,
    Requested,
    Starting,
    Verifying,
    Active,
    Failed,
}

impl ExclusiveHdrPhase {
    /// Live exclusive-HDR phases that may select a connector.
    ///
    /// `Failed` is a latch against automatic exclusive retry. It must not keep
    /// selecting a connector or block ordinary Apply Requested HDR10.
    pub fn selects_output(self) -> bool {
        matches!(
            self,
            Self::Requested | Self::Starting | Self::Verifying | Self::Active
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExclusiveHdrState {
    pub phase: ExclusiveHdrPhase,
    #[serde(default)]
    pub connector: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<u32>,
}

pub fn exclusive_hdr_state_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("exclusive-hdr.json")
}

pub fn load_exclusive_hdr_state() -> ExclusiveHdrState {
    std::fs::read(exclusive_hdr_state_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_exclusive_hdr_state(state: &ExclusiveHdrState) -> std::io::Result<()> {
    let path = exclusive_hdr_state_path();
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "exclusive HDR state path has no parent",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temporary, path)
}

/// Keep a successful exclusive HDR request armed across logout, restart, and
/// shutdown. Those session edges often kill the compositor before the DRM
/// loop can rewrite `Active` back to `Requested`, which previously latched
/// `Failed` and made the next Apply Requested HDR10 do nothing.
pub fn rearm_exclusive_hdr_for_next_session() {
    let mut state = load_exclusive_hdr_state();
    if !state.phase.selects_output() {
        return;
    }
    state.phase = ExclusiveHdrPhase::Requested;
    state.reason = None;
    state.session_id = None;
    let _ = save_exclusive_hdr_state(&state);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSettings {
    pub pointer_speed: f32,
    pub natural_scroll: bool,
    /// XKB layout (e.g. "us", "de"). Fed to the compositor's XkbConfig.
    #[serde(default = "default_keyboard_layout")]
    pub keyboard_layout: String,
    /// XKB variant (e.g. "dvorak"). Empty means none.
    #[serde(default)]
    pub keyboard_variant: String,
    /// XKB model. Empty defers to xkbcommon's own default.
    #[serde(default)]
    pub keyboard_model: String,
    /// XKB options (e.g. "ctrl:nocaps"). Empty means none.
    #[serde(default)]
    pub keyboard_options: String,
    /// Action name to shortcut overrides, for example
    /// `"launch_terminal": "Super+Enter"`.
    #[serde(default)]
    pub keybindings: BTreeMap<String, String>,
}

fn default_keyboard_layout() -> String {
    "us".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub terminal: String,
    pub browser: String,
    #[serde(default)]
    pub browser_launch_backend: BrowserLaunchBackend,
    pub file_manager: String,
    #[serde(default)]
    pub email: String,
    #[serde(default = "default_true")]
    pub pin_email_to_shelf: bool,
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
    #[serde(default = "default_maximize_on_launch")]
    pub maximize_on_launch: bool,
    /// Max number of workspace buttons shown individually in the sidebar before
    /// they collapse into an overflow button. Does not limit how many workspaces
    /// can actually be created.
    #[serde(default = "default_max_workspace_slots")]
    pub max_workspace_slots: u32,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            restore_session: default_restore_session(),
            maximize_on_launch: default_maximize_on_launch(),
            max_workspace_slots: default_max_workspace_slots(),
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
    #[serde(default = "default_notification_history_limit")]
    pub notification_history_limit: u32,
    #[serde(default)]
    pub clear_notification_history_on_logout: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            recent_files: default_recent_files(),
            location_services: default_location_services(),
            hide_lock_screen_notifications: default_hide_lock_screen_notifications(),
            notification_history_limit: default_notification_history_limit(),
            clear_notification_history_on_logout: false,
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
            high_contrast: false,
        },
        displays: DisplaySettings { outputs: vec![] },
        input: InputSettings {
            pointer_speed: 1.0,
            natural_scroll: false,
            keyboard_layout: default_keyboard_layout(),
            keyboard_variant: String::new(),
            keyboard_model: String::new(),
            keyboard_options: String::new(),
            keybindings: BTreeMap::new(),
        },
        apps: AppSettings {
            terminal: "alacritty".into(),
            browser: "google-chrome".into(),
            browser_launch_backend: BrowserLaunchBackend::Auto,
            file_manager: "focaldesk-files".into(),
            email: String::new(),
            pin_email_to_shelf: true,
        },
        workspaces: WorkspaceSettings::default(),
        privacy: PrivacySettings::default(),
        power: PowerSettings::default(),
        debug: DebugSettings::default(),
        chrome: ChromeSettings::default(),
    }
}

fn default_true() -> bool {
    true
}

fn default_recent_files() -> bool {
    true
}

fn default_restore_session() -> bool {
    true
}

fn default_maximize_on_launch() -> bool {
    true
}

fn default_max_workspace_slots() -> u32 {
    4
}

fn default_location_services() -> bool {
    false
}

fn default_hide_lock_screen_notifications() -> bool {
    true
}

fn default_notification_history_limit() -> u32 {
    100
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

    #[test]
    fn keybindings_default_empty_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["input"]
            .as_object_mut()
            .unwrap()
            .remove("keybindings");

        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.input.keybindings.is_empty());

        settings
            .input
            .keybindings
            .insert("launch_terminal".into(), "Ctrl+Alt+T".into());
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(
            restored.input.keybindings.get("launch_terminal"),
            Some(&"Ctrl+Alt+T".to_string())
        );
    }

    #[test]
    fn high_contrast_defaults_off_for_existing_settings() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("high_contrast");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert!(!settings.appearance.high_contrast);
    }

    #[test]
    fn email_shelf_settings_are_backward_compatible() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        let apps = value["apps"].as_object_mut().unwrap();
        apps.remove("email");
        apps.remove("pin_email_to_shelf");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.apps.email.is_empty());
        assert!(settings.apps.pin_email_to_shelf);
    }

    #[test]
    fn chrome_settings_default_for_existing_files_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value.as_object_mut().unwrap().remove("chrome");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.chrome.sidebar.order.is_empty());
        assert!(settings.chrome.topbar.hidden.is_empty());

        settings.chrome.topbar.hidden.push(103);
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(restored.chrome.topbar.hidden, vec![103]);
    }

    #[test]
    fn maximize_on_launch_defaults_true_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value.as_object_mut().unwrap().remove("workspaces");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.workspaces.maximize_on_launch);

        let mut settings = settings;
        settings.workspaces.maximize_on_launch = false;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(!restored.workspaces.maximize_on_launch);
    }

    #[test]
    fn max_workspace_slots_defaults_to_four_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value.as_object_mut().unwrap().remove("workspaces");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.workspaces.max_workspace_slots, 4);

        let mut settings = settings;
        settings.workspaces.max_workspace_slots = 7;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(restored.workspaces.max_workspace_slots, 7);
    }

    #[test]
    fn exclusive_failed_phase_does_not_keep_selecting_a_connector() {
        assert!(ExclusiveHdrPhase::Requested.selects_output());
        assert!(ExclusiveHdrPhase::Active.selects_output());
        assert!(!ExclusiveHdrPhase::Failed.selects_output());
        assert!(!ExclusiveHdrPhase::Disabled.selects_output());
        assert!(!ExclusiveHdrPhase::Off.selects_output());
    }

    #[test]
    fn exclusive_hdr_state_round_trips_failure_context() {
        let state = ExclusiveHdrState {
            phase: ExclusiveHdrPhase::Failed,
            connector: Some("DP-3".into()),
            reason: Some("vblank timeout".into()),
            session_id: Some(42),
        };
        let restored: ExclusiveHdrState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn hdr_appearance_defaults_are_neutral_and_valid() {
        let appearance = HdrAppearance::default();
        assert_eq!(appearance.reference_white_nits, 203.0);
        assert_eq!(appearance.peak_nits, 450.0);
        assert_eq!(appearance.saturation, 1.0);
        assert_eq!(appearance.midtone_gamma, 1.0);
        assert_eq!(appearance.validate(), Ok(appearance));
    }

    #[test]
    fn hdr_appearance_rejects_unsafe_or_non_finite_values() {
        assert!(
            HdrAppearance {
                reference_white_nits: 451.0,
                ..HdrAppearance::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HdrAppearance {
                reference_white_nits: 300.0,
                peak_nits: 250.0,
                ..HdrAppearance::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HdrAppearance {
                saturation: f32::NAN,
                ..HdrAppearance::default()
            }
            .validate()
            .is_err()
        );
    }
}
