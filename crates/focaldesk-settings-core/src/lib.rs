// crates/focaldesk-settings-core/src/lib.rs
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Canonical storage for the typed shell/compositor configuration.
    /// Kept as JSON here so `focaldesk-config` can own its versioned schema
    /// without creating a crate dependency cycle.
    pub desktop_config: serde_json::Value,
    pub appearance: AppearanceSettings,
    pub displays: DisplaySettings,
    pub input: InputSettings,
    #[serde(default)]
    pub accessibility: AccessibilitySettings,
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

impl Default for Settings {
    fn default() -> Self {
        default_settings()
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessibilitySettings {
    pub magnifier_enabled: bool,
    pub magnifier_zoom: f32,
    pub magnifier_follow_focus: bool,
    /// Per-connector zoom overrides populated by compositor shortcuts.
    pub magnifier_output_zoom: BTreeMap<String, f32>,
    pub cursor_size: u32,
    /// A display-only warm color transform, applied after output calibration.
    #[serde(default)]
    pub night_light: NightLightSettings,
    /// A display-only color-vision aid, applied after output calibration.
    #[serde(default)]
    pub color_filter: ColorFilter,
    /// Connector-specific opt-outs for the display comfort transforms.
    #[serde(default)]
    pub display_comfort_disabled_outputs: BTreeMap<String, bool>,
    /// Optional local ambient-light adaptation. It is inactive when no
    /// readable IIO illuminance sensor is present.
    #[serde(default)]
    pub ambient_light_enabled: bool,
    #[serde(default = "default_ambient_min_brightness")]
    pub ambient_min_brightness: f32,
    /// Start the trusted Maliit on-screen keyboard input method for touch and
    /// accessibility text entry.
    #[serde(default)]
    pub on_screen_keyboard_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NightLightSettings {
    pub enabled: bool,
    /// The target white point in kelvin. 6500 is neutral daylight.
    pub temperature_kelvin: u32,
    pub schedule: NightLightSchedule,
    /// Minutes after midnight; used by the custom schedule.
    pub custom_start_minutes: u16,
    pub custom_end_minutes: u16,
    /// Duration of the warm-up and cool-down around schedule boundaries.
    pub transition_minutes: u16,
    /// User-supplied coordinates for local-only solar scheduling. No network
    /// lookup is performed by the compositor.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NightLightSchedule {
    #[default]
    Manual,
    Custom,
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorFilter {
    #[default]
    Off,
    Grayscale,
    Protanopia,
    Deuteranopia,
    Tritanopia,
}

impl Default for NightLightSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            temperature_kelvin: 4500,
            schedule: NightLightSchedule::Manual,
            custom_start_minutes: 19 * 60,
            custom_end_minutes: 7 * 60,
            transition_minutes: 30,
            latitude: None,
            longitude: None,
        }
    }
}

impl AccessibilitySettings {
    pub const MAGNIFIER_ZOOM_RANGE: std::ops::RangeInclusive<f32> = 1.0..=8.0;
    pub const CURSOR_SIZE_RANGE: std::ops::RangeInclusive<u32> = 24..=96;

    pub fn sanitized(mut self) -> Self {
        if !self.magnifier_zoom.is_finite() {
            self.magnifier_zoom = 2.0;
        }
        self.magnifier_zoom = self.magnifier_zoom.clamp(
            *Self::MAGNIFIER_ZOOM_RANGE.start(),
            *Self::MAGNIFIER_ZOOM_RANGE.end(),
        );
        self.magnifier_output_zoom
            .retain(|_, zoom| zoom.is_finite());
        for zoom in self.magnifier_output_zoom.values_mut() {
            *zoom = zoom.clamp(
                *Self::MAGNIFIER_ZOOM_RANGE.start(),
                *Self::MAGNIFIER_ZOOM_RANGE.end(),
            );
        }
        self.cursor_size = self.cursor_size.clamp(
            *Self::CURSOR_SIZE_RANGE.start(),
            *Self::CURSOR_SIZE_RANGE.end(),
        );
        self.night_light.temperature_kelvin =
            self.night_light.temperature_kelvin.clamp(1_000, 6_500);
        self.night_light.custom_start_minutes = self.night_light.custom_start_minutes.min(1_439);
        self.night_light.custom_end_minutes = self.night_light.custom_end_minutes.min(1_439);
        self.night_light.transition_minutes = self.night_light.transition_minutes.min(60);
        self.night_light.latitude = self
            .night_light
            .latitude
            .filter(|value| value.is_finite() && (-90.0..=90.0).contains(value));
        self.night_light.longitude = self
            .night_light
            .longitude
            .filter(|value| value.is_finite() && (-180.0..=180.0).contains(value));
        self.display_comfort_disabled_outputs
            .retain(|_, disabled| *disabled);
        if !self.ambient_min_brightness.is_finite() {
            self.ambient_min_brightness = default_ambient_min_brightness();
        }
        self.ambient_min_brightness = self.ambient_min_brightness.clamp(0.35, 1.0);
        self
    }
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            magnifier_enabled: false,
            magnifier_zoom: 2.0,
            magnifier_follow_focus: true,
            magnifier_output_zoom: BTreeMap::new(),
            cursor_size: 24,
            night_light: NightLightSettings::default(),
            color_filter: ColorFilter::Off,
            display_comfort_disabled_outputs: BTreeMap::new(),
            ambient_light_enabled: false,
            ambient_min_brightness: default_ambient_min_brightness(),
            on_screen_keyboard_enabled: false,
        }
    }
}

fn default_ambient_min_brightness() -> f32 {
    0.55
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
    #[serde(default)]
    pub transform: DisplayTransform,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DisplayTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

/// Per-output creative controls for the final HDR10 encode pass.
///
/// These values never enable HDR or change KMS connector state. They are only
/// consumed after the compositor has independently selected a guarded HDR10
/// output path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrAppearance {
    pub black_level_nits: f32,
    pub reference_white_nits: f32,
    pub peak_nits: f32,
    #[serde(default = "legacy_hdr_full_frame_peak_nits")]
    pub full_frame_peak_nits: f32,
    pub saturation: f32,
    pub midtone_gamma: f32,
    /// Highlight roll-off used by the final HDR output transform.
    pub tone_mapper: HdrToneMapper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HdrToneMapper {
    /// A display-peak-aware, BT.2390-inspired shoulder. This preserves the
    /// historical FocalDesk output and is the safest general-purpose default.
    #[default]
    Bt2390,
    Reinhard,
    Hable,
    Aces,
}

impl HdrToneMapper {
    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Bt2390 => 0.0,
            Self::Reinhard => 1.0,
            Self::Hable => 2.0,
            Self::Aces => 3.0,
        }
    }
}

fn legacy_hdr_full_frame_peak_nits() -> f32 {
    // Valid for every legacy peak setting; users can then measure upward.
    203.0
}

/// Session-only absolute-luminance stimulus shown by the final HDR10 encoder.
/// This is intentionally not part of `DisplayConfig`, so calibration cannot
/// accidentally replace the desktop after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HdrCalibrationPattern {
    #[default]
    Off,
    Overview,
    NearBlack,
    ReferenceWhite,
    PeakWindow,
    PeakFullFrame,
}

impl HdrCalibrationPattern {
    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Overview => 1.0,
            Self::NearBlack => 2.0,
            Self::ReferenceWhite => 3.0,
            Self::PeakWindow => 4.0,
            Self::PeakFullFrame => 5.0,
        }
    }
}

impl Default for HdrAppearance {
    fn default() -> Self {
        Self {
            black_level_nits: 0.05,
            reference_white_nits: 203.0,
            peak_nits: 450.0,
            full_frame_peak_nits: 300.0,
            saturation: 1.0,
            midtone_gamma: 1.0,
            tone_mapper: HdrToneMapper::Bt2390,
        }
    }
}

impl HdrAppearance {
    // PQ and CTA-861 metadata support values through 10,000 nits. Individual
    // outputs start from their EDID values and can then be calibrated by the
    // user; these bounds are format limits, not a panel-specific policy.
    pub const REFERENCE_WHITE_RANGE: std::ops::RangeInclusive<f32> = 80.0..=10_000.0;
    pub const PEAK_RANGE: std::ops::RangeInclusive<f32> = 203.0..=10_000.0;
    pub const BLACK_LEVEL_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.25;
    pub const FULL_FRAME_PEAK_RANGE: std::ops::RangeInclusive<f32> = 80.0..=10_000.0;
    pub const SATURATION_RANGE: std::ops::RangeInclusive<f32> = 0.75..=1.25;
    pub const MIDTONE_GAMMA_RANGE: std::ops::RangeInclusive<f32> = 0.70..=1.50;

    /// Neutral per-monitor starting point derived from CTA-861 HDR static
    /// metadata. EDID values are capabilities, not a calibration, so the UI
    /// still permits the user to tune and persist them for this monitor.
    pub fn from_edid(max_luminance_nits: f32, max_fall_nits: f32, min_luminance_nits: f32) -> Self {
        let peak_nits = max_luminance_nits.clamp(203.0, 10_000.0);
        Self {
            black_level_nits: min_luminance_nits.clamp(0.0, 0.25),
            reference_white_nits: 203.0_f32.min(peak_nits),
            peak_nits,
            full_frame_peak_nits: max_fall_nits.clamp(80.0, peak_nits),
            saturation: 1.0,
            midtone_gamma: 1.0,
            tone_mapper: HdrToneMapper::Bt2390,
        }
    }

    pub fn validate(self) -> Result<Self, &'static str> {
        if !self.black_level_nits.is_finite()
            || !self.reference_white_nits.is_finite()
            || !self.peak_nits.is_finite()
            || !self.full_frame_peak_nits.is_finite()
            || !self.saturation.is_finite()
            || !self.midtone_gamma.is_finite()
        {
            return Err("HDR appearance values must be finite");
        }
        if !Self::BLACK_LEVEL_RANGE.contains(&self.black_level_nits) {
            return Err("HDR black level is outside the supported range");
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
        if !Self::FULL_FRAME_PEAK_RANGE.contains(&self.full_frame_peak_nits) {
            return Err("HDR full-frame peak is outside the supported range");
        }
        if self.full_frame_peak_nits > self.peak_nits {
            return Err("HDR full-frame peak cannot exceed peak luminance");
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
    /// Finger count reserved for horizontal workspace swipes. Zero disables
    /// compositor-owned workspace gestures so applications receive them.
    #[serde(default = "default_workspace_swipe_fingers")]
    pub workspace_swipe_fingers: u32,
    /// Accumulated logical-pixel distance required to change workspaces.
    #[serde(default = "default_workspace_swipe_distance")]
    pub workspace_swipe_distance: f64,
    /// Per-device libinput overrides keyed by the compositor's stable,
    /// hardware-derived device identifier. Missing fields preserve libinput's
    /// current/default behavior.
    #[serde(default)]
    pub devices: BTreeMap<String, InputDeviceSettings>,
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
    /// Delay before a held key begins repeating, in milliseconds.
    #[serde(default = "default_keyboard_repeat_delay_ms")]
    pub keyboard_repeat_delay_ms: u32,
    /// Number of repeated key events generated per second.
    #[serde(default = "default_keyboard_repeat_rate")]
    pub keyboard_repeat_rate: u32,
    /// Action name to shortcut overrides, for example
    /// `"launch_terminal": "Super+Enter"`.
    #[serde(default)]
    pub keybindings: BTreeMap<String, String>,
    /// Keyboard filters implemented by the compositor before events reach
    /// Wayland or XWayland clients.
    #[serde(default)]
    pub accessibility: KeyboardAccessibilitySettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardAccessibilitySettings {
    pub sticky_keys: bool,
    pub slow_keys: bool,
    pub slow_keys_delay_ms: u32,
    pub bounce_keys: bool,
    pub bounce_keys_delay_ms: u32,
    /// Offer to toggle Sticky Keys after Shift is pressed five times.
    pub sticky_keys_shortcut: bool,
    /// Show short desktop notifications when filters or latched modifiers change.
    pub visual_feedback: bool,
}

impl KeyboardAccessibilitySettings {
    pub const SLOW_KEYS_DELAY_RANGE: std::ops::RangeInclusive<u32> = 100..=2_000;
    pub const BOUNCE_KEYS_DELAY_RANGE: std::ops::RangeInclusive<u32> = 100..=2_000;

    pub fn sanitized(mut self) -> Self {
        self.slow_keys_delay_ms = self.slow_keys_delay_ms.clamp(
            *Self::SLOW_KEYS_DELAY_RANGE.start(),
            *Self::SLOW_KEYS_DELAY_RANGE.end(),
        );
        self.bounce_keys_delay_ms = self.bounce_keys_delay_ms.clamp(
            *Self::BOUNCE_KEYS_DELAY_RANGE.start(),
            *Self::BOUNCE_KEYS_DELAY_RANGE.end(),
        );
        self
    }
}

impl Default for KeyboardAccessibilitySettings {
    fn default() -> Self {
        Self {
            sticky_keys: false,
            slow_keys: false,
            slow_keys_delay_ms: 500,
            bounce_keys: false,
            bounce_keys_delay_ms: 300,
            sticky_keys_shortcut: true,
            visual_feedback: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputDeviceSettings {
    pub enabled: Option<bool>,
    pub tap_to_click: Option<bool>,
    pub disable_while_typing: Option<bool>,
    pub acceleration_profile: InputAccelerationProfile,
    pub acceleration_speed: Option<f64>,
    pub scroll_method: InputScrollMethod,
    pub left_handed: Option<bool>,
    pub middle_button_emulation: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputAccelerationProfile {
    #[default]
    Default,
    Adaptive,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputScrollMethod {
    #[default]
    Default,
    TwoFinger,
    Edge,
    OnButtonDown,
    Disabled,
}

fn default_keyboard_layout() -> String {
    "us".into()
}

fn default_workspace_swipe_fingers() -> u32 {
    3
}

fn default_workspace_swipe_distance() -> f64 {
    120.0
}

fn default_keyboard_repeat_delay_ms() -> u32 {
    200
}

fn default_keyboard_repeat_rate() -> u32 {
    25
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
    /// Ordered placement defaults for newly created application windows.
    /// The first matching rule wins; restored windows retain their session
    /// placement instead.
    #[serde(default)]
    pub window_rules: Vec<WindowRule>,
    /// Programs launched once after a new FocalDesk session starts.
    #[serde(default)]
    pub startup_apps: Vec<StartupApp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StartupApp {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl StartupApp {
    pub fn sanitized(mut self) -> Option<Self> {
        self.name = self.name.trim().chars().take(128).collect();
        self.command = self.command.trim().chars().take(1_024).collect();
        self.arguments = self
            .arguments
            .into_iter()
            .map(|argument| argument.trim().chars().take(512).collect::<String>())
            .filter(|argument| !argument.is_empty())
            .take(32)
            .collect();
        (!self.command.is_empty()).then_some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowRule {
    /// A Wayland application ID or X11 WM_CLASS. Matching is case-insensitive.
    pub app_id: String,
    /// Optional case-insensitive substring match against the window title.
    pub title_contains: String,
    /// One-based workspace number. Omit to leave the current workspace alone.
    pub workspace: Option<u32>,
    /// Stable output connector name, such as `HDMI-A-1`.
    pub output: Option<String>,
    /// Whether the window starts floating rather than tiled.
    pub floating: Option<bool>,
    /// Whether the window starts maximized.
    pub maximize: Option<bool>,
}

impl WindowRule {
    pub fn sanitized(mut self) -> Option<Self> {
        self.app_id = self.app_id.trim().chars().take(256).collect();
        self.title_contains = self.title_contains.trim().chars().take(256).collect();
        self.output = self.output.and_then(|output| {
            let output: String = output.trim().chars().take(256).collect();
            (!output.is_empty()).then_some(output)
        });
        self.workspace = self.workspace.filter(|workspace| *workspace > 0);
        (!self.app_id.is_empty() || !self.title_contains.is_empty()).then_some(self)
    }

    pub fn matches(&self, app_id: Option<&str>, title: &str) -> bool {
        let app_matches = self.app_id.is_empty()
            || app_id.is_some_and(|candidate| candidate.eq_ignore_ascii_case(&self.app_id));
        let title_matches = self.title_contains.is_empty()
            || title
                .to_ascii_lowercase()
                .contains(&self.title_contains.to_ascii_lowercase());
        app_matches && title_matches
    }
}

impl AppSettings {
    pub fn sanitized(mut self) -> Self {
        self.window_rules = self
            .window_rules
            .into_iter()
            .filter_map(WindowRule::sanitized)
            .take(64)
            .collect();
        self.startup_apps = self
            .startup_apps
            .into_iter()
            .filter_map(StartupApp::sanitized)
            .take(64)
            .collect();
        self
    }
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
    /// Restore saved split pane assignments as part of session restoration.
    /// Kept separate because recreating application tiling is more intrusive
    /// than reopening applications on their previous workspaces.
    #[serde(default)]
    pub restore_split_layouts: bool,
    #[serde(default = "default_maximize_on_launch")]
    pub maximize_on_launch: bool,
    /// Enables optional split-screen window placement controls. Consumers must
    /// keep those controls hidden or inactive while this is false.
    #[serde(default)]
    pub split_screen_enabled: bool,
    /// Last draggable split boundaries, keyed by display connector. Values are
    /// normalized so they remain useful when a display mode changes.
    #[serde(default)]
    pub split_ratios: BTreeMap<String, SplitRatioSettings>,
    /// Max number of workspace buttons shown individually in the sidebar before
    /// they collapse into an overflow button. Does not limit how many workspaces
    /// can actually be created.
    #[serde(default = "default_max_workspace_slots")]
    pub max_workspace_slots: u32,
    /// Optional local image paths keyed by one-based workspace number. Missing
    /// entries inherit the active theme's wallpaper.
    #[serde(default)]
    pub workspace_wallpapers: BTreeMap<u32, String>,
    /// Optional user labels keyed by one-based workspace number.
    #[serde(default)]
    pub workspace_names: BTreeMap<u32, String>,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            restore_session: default_restore_session(),
            restore_split_layouts: false,
            maximize_on_launch: default_maximize_on_launch(),
            split_screen_enabled: false,
            split_ratios: BTreeMap::new(),
            max_workspace_slots: default_max_workspace_slots(),
            workspace_wallpapers: BTreeMap::new(),
            workspace_names: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitRatioSettings {
    pub side_by_side: f64,
    pub stacked: f64,
}

impl Default for SplitRatioSettings {
    fn default() -> Self {
        Self {
            side_by_side: 0.5,
            stacked: 0.5,
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
        desktop_config: serde_json::Value::Null,
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
            workspace_swipe_fingers: default_workspace_swipe_fingers(),
            workspace_swipe_distance: default_workspace_swipe_distance(),
            devices: BTreeMap::new(),
            keyboard_layout: default_keyboard_layout(),
            keyboard_variant: String::new(),
            keyboard_model: String::new(),
            keyboard_options: String::new(),
            keyboard_repeat_delay_ms: default_keyboard_repeat_delay_ms(),
            keyboard_repeat_rate: default_keyboard_repeat_rate(),
            keybindings: BTreeMap::new(),
            accessibility: KeyboardAccessibilitySettings::default(),
        },
        accessibility: AccessibilitySettings::default(),
        apps: AppSettings {
            terminal: "alacritty".into(),
            browser: "google-chrome".into(),
            browser_launch_backend: BrowserLaunchBackend::Auto,
            file_manager: "focaldesk-files".into(),
            email: String::new(),
            pin_email_to_shelf: true,
            window_rules: Vec::new(),
            startup_apps: Vec::new(),
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

    let mut settings = match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|_| default_settings()),
        Err(_) => default_settings(),
    };
    settings.input.accessibility = settings.input.accessibility.sanitized();
    settings.accessibility = settings.accessibility.sanitized();
    settings.apps = settings.apps.sanitized();
    settings
}

pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let path = settings_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = match fs::read_to_string(&path) {
        Ok(json) => Some(json),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let json = merged_settings_json(settings, existing.as_deref())?;
    fs::write(path, json)
}

fn merged_settings_json(settings: &Settings, existing: Option<&str>) -> std::io::Result<String> {
    let mut settings = settings.clone();
    settings.input.accessibility = settings.input.accessibility.sanitized();
    settings.accessibility = settings.accessibility.sanitized();
    settings.apps = settings.apps.sanitized();
    let mut value = serde_json::to_value(settings).map_err(std::io::Error::other)?;
    if let Some(existing) = existing {
        let existing: serde_json::Value = serde_json::from_str(existing)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let existing = existing.as_object().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "settings.json must contain a JSON object",
            )
        })?;
        if let Some(desktop_config) = existing.get("desktop_config") {
            value
                .as_object_mut()
                .expect("serialized Settings is an object")
                .insert("desktop_config".into(), desktop_config.clone());
        }
    }
    let mut json = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_save_merge_preserves_live_desktop_config() {
        let settings = default_settings();
        let json = merged_settings_json(
            &settings,
            Some(r#"{"desktop_config":{"dock":{"position":"right"}}}"#),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["desktop_config"]["dock"]["position"], "right");
    }

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
    fn split_layout_restore_defaults_disabled_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["workspaces"]
            .as_object_mut()
            .unwrap()
            .remove("restore_split_layouts");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(!settings.workspaces.restore_split_layouts);

        settings.workspaces.restore_split_layouts = true;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(restored.workspaces.restore_split_layouts);
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
    fn keyboard_repeat_settings_are_backward_compatible_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        let input = value["input"].as_object_mut().unwrap();
        input.remove("keyboard_repeat_delay_ms");
        input.remove("keyboard_repeat_rate");

        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.input.keyboard_repeat_delay_ms, 200);
        assert_eq!(settings.input.keyboard_repeat_rate, 25);

        settings.input.keyboard_repeat_delay_ms = 450;
        settings.input.keyboard_repeat_rate = 40;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(restored.input.keyboard_repeat_delay_ms, 450);
        assert_eq!(restored.input.keyboard_repeat_rate, 40);
    }

    #[test]
    fn keyboard_accessibility_defaults_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["input"]
            .as_object_mut()
            .unwrap()
            .remove("accessibility");

        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(
            settings.input.accessibility,
            KeyboardAccessibilitySettings::default()
        );

        settings.input.accessibility.sticky_keys = true;
        settings.input.accessibility.slow_keys = true;
        settings.input.accessibility.slow_keys_delay_ms = 750;
        settings.input.accessibility.bounce_keys = true;
        settings.input.accessibility.bounce_keys_delay_ms = 400;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(restored.input.accessibility.sticky_keys);
        assert!(restored.input.accessibility.slow_keys);
        assert_eq!(restored.input.accessibility.slow_keys_delay_ms, 750);
        assert!(restored.input.accessibility.bounce_keys);
        assert_eq!(restored.input.accessibility.bounce_keys_delay_ms, 400);
    }

    #[test]
    fn keyboard_accessibility_sanitizes_delays() {
        let settings = KeyboardAccessibilitySettings {
            slow_keys_delay_ms: 0,
            bounce_keys_delay_ms: u32::MAX,
            ..KeyboardAccessibilitySettings::default()
        }
        .sanitized();
        assert_eq!(settings.slow_keys_delay_ms, 100);
        assert_eq!(settings.bounce_keys_delay_ms, 2_000);
    }

    #[test]
    fn visual_accessibility_defaults_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value.as_object_mut().unwrap().remove("accessibility");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.accessibility, AccessibilitySettings::default());

        settings.accessibility.magnifier_enabled = true;
        settings.accessibility.magnifier_zoom = 3.5;
        settings.accessibility.cursor_size = 64;
        settings
            .accessibility
            .magnifier_output_zoom
            .insert("DP-1".into(), 4.0);
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(restored.accessibility.magnifier_enabled);
        assert_eq!(restored.accessibility.magnifier_zoom, 3.5);
        assert_eq!(restored.accessibility.cursor_size, 64);
        assert_eq!(
            restored.accessibility.magnifier_output_zoom.get("DP-1"),
            Some(&4.0)
        );
    }

    #[test]
    fn visual_accessibility_sanitizes_zoom_and_cursor_size() {
        let mut settings = AccessibilitySettings {
            magnifier_zoom: f32::NAN,
            cursor_size: u32::MAX,
            ..AccessibilitySettings::default()
        };
        settings
            .magnifier_output_zoom
            .insert("HDMI-A-1".into(), 99.0);
        settings
            .magnifier_output_zoom
            .insert("broken".into(), f32::INFINITY);
        let settings = settings.sanitized();
        assert_eq!(settings.magnifier_zoom, 2.0);
        assert_eq!(settings.cursor_size, 96);
        assert_eq!(settings.magnifier_output_zoom.get("HDMI-A-1"), Some(&8.0));
        assert!(!settings.magnifier_output_zoom.contains_key("broken"));
    }

    #[test]
    fn display_comfort_settings_are_sanitized_and_round_trip() {
        let settings = AccessibilitySettings {
            night_light: NightLightSettings {
                enabled: true,
                temperature_kelvin: 99_999,
                schedule: NightLightSchedule::Custom,
                custom_start_minutes: u16::MAX,
                custom_end_minutes: u16::MAX,
                ..NightLightSettings::default()
            },
            color_filter: ColorFilter::Deuteranopia,
            ..AccessibilitySettings::default()
        }
        .sanitized();
        assert_eq!(settings.night_light.temperature_kelvin, 6_500);
        assert_eq!(settings.night_light.custom_start_minutes, 1_439);
        assert_eq!(settings.color_filter, ColorFilter::Deuteranopia);
        let restored: AccessibilitySettings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(restored.color_filter, ColorFilter::Deuteranopia);
    }

    #[test]
    fn pointer_gesture_settings_are_backward_compatible_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        let input = value["input"].as_object_mut().unwrap();
        input.remove("workspace_swipe_fingers");
        input.remove("workspace_swipe_distance");

        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.input.workspace_swipe_fingers, 3);
        assert_eq!(settings.input.workspace_swipe_distance, 120.0);

        settings.input.workspace_swipe_fingers = 4;
        settings.input.workspace_swipe_distance = 180.0;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(restored.input.workspace_swipe_fingers, 4);
        assert_eq!(restored.input.workspace_swipe_distance, 180.0);
    }

    #[test]
    fn per_device_input_overrides_default_empty_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["input"].as_object_mut().unwrap().remove("devices");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.input.devices.is_empty());

        settings.input.devices.insert(
            "046d:c52b:mouse".into(),
            InputDeviceSettings {
                enabled: Some(true),
                acceleration_profile: InputAccelerationProfile::Flat,
                acceleration_speed: Some(-0.25),
                left_handed: Some(true),
                ..Default::default()
            },
        );
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        let device = &restored.input.devices["046d:c52b:mouse"];
        assert_eq!(device.enabled, Some(true));
        assert_eq!(device.acceleration_profile, InputAccelerationProfile::Flat);
        assert_eq!(device.acceleration_speed, Some(-0.25));
        assert_eq!(device.left_handed, Some(true));
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
    fn window_rules_match_and_discard_empty_entries() {
        let rules = vec![
            WindowRule {
                app_id: "  org.example.Editor  ".into(),
                title_contains: "document".into(),
                workspace: Some(2),
                output: Some(" HDMI-A-1 ".into()),
                floating: Some(true),
                maximize: Some(false),
            },
            WindowRule::default(),
        ];
        let settings = AppSettings {
            terminal: "terminal".into(),
            browser: "browser".into(),
            browser_launch_backend: BrowserLaunchBackend::Auto,
            file_manager: "files".into(),
            email: String::new(),
            pin_email_to_shelf: true,
            window_rules: rules,
            startup_apps: Vec::new(),
        }
        .sanitized();

        assert_eq!(settings.window_rules.len(), 1);
        let rule = &settings.window_rules[0];
        assert_eq!(rule.output.as_deref(), Some("HDMI-A-1"));
        assert!(rule.matches(Some("ORG.EXAMPLE.EDITOR"), "Untitled document"));
        assert!(!rule.matches(Some("org.example.Editor"), "Other"));
    }

    #[test]
    fn startup_apps_discard_blank_commands_and_sanitize_arguments() {
        let settings = AppSettings {
            terminal: "terminal".into(),
            browser: "browser".into(),
            browser_launch_backend: BrowserLaunchBackend::Auto,
            file_manager: "files".into(),
            email: String::new(),
            pin_email_to_shelf: true,
            window_rules: Vec::new(),
            startup_apps: vec![
                StartupApp {
                    name: "  Files  ".into(),
                    command: " focaldesk-files ".into(),
                    arguments: vec!["  --new-window ".into(), String::new()],
                    enabled: true,
                },
                StartupApp::default(),
            ],
        }
        .sanitized();

        assert_eq!(settings.startup_apps.len(), 1);
        assert_eq!(settings.startup_apps[0].name, "Files");
        assert_eq!(settings.startup_apps[0].command, "focaldesk-files");
        assert_eq!(settings.startup_apps[0].arguments, ["--new-window"]);
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
    fn workspace_wallpapers_default_empty_and_round_trip() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["workspaces"]
            .as_object_mut()
            .unwrap()
            .remove("workspace_wallpapers");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.workspaces.workspace_wallpapers.is_empty());

        settings
            .workspaces
            .workspace_wallpapers
            .insert(2, "/tmp/workspace-two.png".into());
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(
            restored.workspaces.workspace_wallpapers.get(&2),
            Some(&"/tmp/workspace-two.png".to_string())
        );
    }

    #[test]
    fn split_screen_defaults_disabled_and_round_trips() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["workspaces"]
            .as_object_mut()
            .unwrap()
            .remove("split_screen_enabled");

        let settings: Settings = serde_json::from_value(value).unwrap();
        assert!(!settings.workspaces.split_screen_enabled);

        let mut settings = settings;
        settings.workspaces.split_screen_enabled = true;
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(restored.workspaces.split_screen_enabled);
    }

    #[test]
    fn split_ratios_default_empty_and_round_trip_by_connector() {
        let mut value = serde_json::to_value(default_settings()).unwrap();
        value["workspaces"]
            .as_object_mut()
            .unwrap()
            .remove("split_ratios");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert!(settings.workspaces.split_ratios.is_empty());

        settings.workspaces.split_ratios.insert(
            "DP-1".into(),
            SplitRatioSettings {
                side_by_side: 2.0 / 3.0,
                stacked: 0.4,
            },
        );
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        let ratio = restored.workspaces.split_ratios.get("DP-1").unwrap();
        assert!((ratio.side_by_side - 2.0 / 3.0).abs() < f64::EPSILON);
        assert!((ratio.stacked - 0.4).abs() < f64::EPSILON);
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
        assert_eq!(appearance.black_level_nits, 0.05);
        assert_eq!(appearance.reference_white_nits, 203.0);
        assert_eq!(appearance.peak_nits, 450.0);
        assert_eq!(appearance.full_frame_peak_nits, 300.0);
        assert_eq!(appearance.saturation, 1.0);
        assert_eq!(appearance.midtone_gamma, 1.0);
        assert_eq!(appearance.tone_mapper, HdrToneMapper::Bt2390);
        assert_eq!(appearance.validate(), Ok(appearance));
    }

    #[test]
    fn hdr_appearance_uses_per_monitor_edid_luminance() {
        let appearance = HdrAppearance::from_edid(1_405.0, 951.0, 0.0);
        assert_eq!(appearance.reference_white_nits, 203.0);
        assert_eq!(appearance.peak_nits, 1_405.0);
        assert_eq!(appearance.full_frame_peak_nits, 951.0);
        assert_eq!(appearance.black_level_nits, 0.0);
        assert!(appearance.validate().is_ok());
    }

    #[test]
    fn hdr_appearance_old_json_receives_calibration_defaults() {
        let appearance: HdrAppearance = serde_json::from_value(serde_json::json!({
            "reference_white_nits": 203.0,
            "peak_nits": 400.0,
            "saturation": 1.0,
            "midtone_gamma": 1.0
        }))
        .unwrap();
        assert_eq!(appearance.black_level_nits, 0.05);
        assert_eq!(appearance.full_frame_peak_nits, 203.0);
        assert_eq!(appearance.peak_nits, 400.0);
        assert_eq!(appearance.tone_mapper, HdrToneMapper::Bt2390);
        assert!(appearance.validate().is_ok());
    }

    #[test]
    fn hdr_appearance_rejects_unsafe_or_non_finite_values() {
        assert!(HdrAppearance {
            reference_white_nits: 451.0,
            ..HdrAppearance::default()
        }
        .validate()
        .is_err());
        assert!(HdrAppearance {
            reference_white_nits: 300.0,
            peak_nits: 250.0,
            ..HdrAppearance::default()
        }
        .validate()
        .is_err());
        assert!(HdrAppearance {
            saturation: f32::NAN,
            ..HdrAppearance::default()
        }
        .validate()
        .is_err());
    }
}
