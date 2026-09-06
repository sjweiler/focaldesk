use adw::prelude::*;
use focaldesk_ai::{list_ai_permission_records, revoke_ai_permission, AiPermissionRecord};
use focaldesk_bluetooth::{load_snapshot as load_bluetooth_snapshot, BluetoothSnapshot};
use focaldesk_config::{load_config, save_config, DockVisibility, FocalDeskConfig};
use focaldesk_gtk::{StateKind, StatusBanner};
use focaldesk_ipc::{
    send_desktop_config, send_desktop_request, send_desktop_set, send_power_request,
    send_settings_request, watch_desktop_keys, DesktopAction, DisplayRuntimeOutputStatus,
    IpcRequest, IpcResponse, PowerIpcRequest, PowerIpcResponse, ThemeEditorCommand,
    THEME_EDITOR_PROTOCOL_VERSION,
};
use focaldesk_ipc::{send_notification_request, NotificationIpcRequest};
use focaldesk_logging::{init_default_logging, session_id};
use focaldesk_permissions::{
    PermissionDecision, PermissionResource, PermissionScope, PermissionTarget,
};
use focaldesk_settings_core::{
    load_exclusive_hdr_state, load_settings, save_exclusive_hdr_state, save_settings,
    BrowserLaunchBackend, DebugLogLevel, DisplayColorProfile, ExclusiveHdrPhase, ExclusiveHdrState,
    HdrAppearance, HdrCalibrationPattern, LidCloseAction, LowBatteryAction, OutputConfig,
    PerformanceMode, PowerButtonAction, Settings,
};
use focaldesk_sounds::{generate_ui_sound, SoundBuffer, UiSound, UiSoundPlayer, SAMPLE_RATE};
use focaldesk_themes::{
    gtk_app_css, gtk_app_prefers_dark, theme_by_name, GradientInterpolation, GradientStop,
    GtkAppThemeOptions, InteractionState, SemanticTheme, SurfaceStyle, ThemeColor, ThemeColorSpace,
    ThemeDocument, ThemeDynamicRange, ThemePackage, ThemePaint, ThemePaintIntent, ThemeWallpaper,
    ThemeWallpaperFit,
};

use gtk::cairo;
use gtk::glib;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

mod location_permissions;

use location_permissions::{
    list_location_permission_records, revoke_location_permission, LocationPermissionRecord,
};

const SCALE_OPTIONS: &[(&str, f64)] = &[
    ("100 %", 1.0),
    ("125 %", 1.25),
    ("133 %", 1.3333334),
    ("150 %", 1.5),
    ("166 %", 1.6666667),
    ("200 %", 2.0),
    ("250 %", 2.5),
    ("266 %", 2.6666667),
];

const THEME_OPTIONS: &[&str] = &["Default", "Eagle", "Moonbase", "Classic"];
const TASK_SHELF_VISIBILITY_OPTIONS: &[&str] = &["Intelligent dodge", "Always visible", "Autohide"];
const ORIENTATION_OPTIONS: &[&str] = &[
    "Landscape",
    "Portrait Right",
    "Landscape Flipped",
    "Portrait Left",
];
const OUTPUT_CONFIGURATION_OPTIONS: &[&str] = &["HiFi 2.0 channels", "Stereo", "Mono"];
const ALERT_SOUND_OPTIONS: &[&str] = &["Default", "Click", "Chime", "None"];
const KEYBOARD_LAYOUT_OPTIONS: &[&str] = &["English (US)", "English (UK)", "German", "French"];
const MODIFIER_BEHAVIOR_OPTIONS: &[&str] = &["Default", "Caps Lock as Ctrl", "Swap Ctrl and Alt"];
const LOG_LEVEL_OPTIONS: &[&str] = &["Error", "Warn", "Info", "Debug", "Trace"];
const POWER_TIMEOUT_OPTIONS: &[&str] =
    &["Never", "1 minute", "5 minutes", "10 minutes", "30 minutes"];
const POWER_TIMEOUT_VALUES: &[Option<u32>] = &[None, Some(1), Some(5), Some(10), Some(30)];
const SUSPEND_TIMEOUT_OPTIONS: &[&str] = &["Never", "15 minutes", "30 minutes", "1 hour"];
const SUSPEND_TIMEOUT_VALUES: &[Option<u32>] = &[None, Some(15), Some(30), Some(60)];
const POWER_BUTTON_OPTIONS: &[&str] = &["Show power menu", "Suspend", "Power off", "Do nothing"];
const LID_CLOSE_OPTIONS: &[&str] = &["Suspend", "Blank screen", "Lock screen", "Do nothing"];
const LOW_BATTERY_OPTIONS: &[&str] = &["Notify only", "Suspend", "Hibernate", "Power off"];
const PERFORMANCE_MODE_OPTIONS: &[&str] = &["Balanced", "Performance", "Power saver"];
const BROWSER_LAUNCH_BACKEND_OPTIONS: &[&str] = &["Auto", "Wayland", "XWayland"];
const DISPLAY_COLOR_PROFILE_OPTIONS: &[&str] = &["Auto", "sRGB", "Display P3"];
const HDR_APPEARANCE_PRESET_OPTIONS: &[&str] =
    &["Neutral (BT.2408)", "Bright room", "Punchy OLED", "Custom"];
const HDR_APPEARANCE_CUSTOM_PRESET: u32 = 3;
const HDR_CALIBRATION_PATTERN_OPTIONS: &[&str] = &[
    "Off",
    "Overview",
    "Near-black steps",
    "Reference-white field",
    "10% peak window",
    "Full-field peak",
];
const EDITABLE_KEYBINDINGS: &[(&str, &str, &str)] = &[
    ("launch_terminal", "Open terminal", "Super+Enter"),
    ("launch_browser", "Open browser", "Super+B"),
    ("toggle_launcher", "Open launcher", "Ctrl+Alt+D"),
    ("close_focused", "Close focused window", "Super+Q"),
    ("lock_screen", "Lock screen", "Super+L"),
    ("toggle_clipboard_history", "Clipboard history", "Super+V"),
    ("focus_previous", "Focus previous window", "Super+F7"),
    ("focus_next", "Focus next window", "Super+F8"),
];

#[derive(Debug, Clone)]
struct WifiNetwork {
    active: bool,
    ssid: String,
    security: String,
    signal: u8,
}

#[derive(Debug, Clone)]
struct WifiSnapshot {
    enabled: bool,
    networks: Vec<WifiNetwork>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct EthernetDevice {
    device: String,
    state: String,
    connection: Option<String>,
}

#[derive(Debug, Clone)]
struct EthernetSnapshot {
    devices: Vec<EthernetDevice>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct Printer {
    name: String,
    enabled: bool,
    accepting_jobs: bool,
    is_default: bool,
    state: String,
    device_uri: Option<String>,
}

#[derive(Debug, Clone)]
struct InstallablePrinter {
    kind: String,
    uri: String,
    suggested_name: String,
}

#[derive(Debug, Clone)]
struct PrinterSnapshot {
    scheduler_running: bool,
    printers: Vec<Printer>,
    installable_printers: Vec<InstallablePrinter>,
    error: Option<String>,
}

#[derive(Debug)]
struct ConfigEvent {
    key: String,
    value: serde_json::Value,
}

type DynamicRows = Rc<RefCell<Vec<gtk::Widget>>>;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct DisplayModeConfig {
    width: i32,
    height: i32,
    refresh_mhz: i32,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct DisplayConfig {
    name: String,
    enabled: bool,

    mode_width: i32,
    mode_height: i32,
    refresh_mhz: i32,
    #[serde(default)]
    available_modes: Vec<DisplayModeConfig>,

    scale: f64,

    logical_x: i32,
    logical_y: i32,

    physical_width_mm: Option<i32>,
    physical_height_mm: Option<i32>,

    primary: bool,
    transform: String,

    #[serde(default)]
    hdr_supported: bool,
    #[serde(default)]
    hdr_requested: bool,
    #[serde(default)]
    hdr_enabled: bool,
    #[serde(default)]
    hdr_appearance: HdrAppearance,
    #[serde(default)]
    color_profile: DisplayColorProfile,
    #[serde(default)]
    icc_profile_path: Option<String>,
    #[serde(skip)]
    icc_lut_fallback_active: bool,
    #[serde(skip)]
    wide_gamut_active: bool,
    #[serde(skip)]
    exclusive_hdr_phase: ExclusiveHdrPhase,
    #[serde(skip)]
    exclusive_hdr_reason: Option<String>,
}

fn display_outputs(displays: &[DisplayConfig]) -> Vec<OutputConfig> {
    displays.iter().map(output_config_from_display).collect()
}

fn persist_displays(displays: &[DisplayConfig]) {
    let path = displays_path();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(text) = serde_json::to_string_pretty(displays) {
        let _ = std::fs::write(path, text);
    }

    let request = IpcRequest::SetDisplays {
        outputs: display_outputs(displays),
    };

    match send_settings_request(&request) {
        Ok(IpcResponse::Ok) => {}
        Ok(IpcResponse::Error { message }) => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                message = %message,
                "settings IPC update rejected"
            );
        }
        Ok(other) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                response = ?other,
                "unexpected settings IPC response"
            );
        }
        Err(err) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "settings IPC unavailable; saved display config directly"
            );
        }
    }
}

fn apply_displays_to_desktop(displays: &[DisplayConfig]) -> Result<(), String> {
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: display_outputs(displays),
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected desktop display response: {other:?}")),
        Err(err) => Err(err),
    }
}

fn save_displays(displays: &[DisplayConfig]) {
    persist_displays(displays);
    if let Err(err) = apply_displays_to_desktop(displays) {
        info!(
            target: "focaldesk",
            session_id = session_id(),
            error = %err,
            "desktop display IPC unavailable; persisted display config for the next session"
        );
    }
}

fn output_config_from_display(display: &DisplayConfig) -> OutputConfig {
    OutputConfig {
        connector: display.name.clone(),
        enabled: display.enabled,
        x: display.logical_x,
        y: display.logical_y,
        width: display.mode_width,
        height: display.mode_height,
        refresh_mhz: display.refresh_mhz,
        scale: display.scale as f32,
        primary: display.primary,
        color_profile: display.color_profile,
        icc_profile_path: display.icc_profile_path.clone(),
        hdr_requested: display.hdr_requested,
        hdr_enabled: display.hdr_enabled,
        hdr_appearance: display.hdr_appearance,
    }
}

fn set_live_hdr_appearance(connector: &str, appearance: HdrAppearance) -> Result<(), String> {
    appearance.validate().map_err(ToString::to_string)?;
    match send_desktop_request(&IpcRequest::SetHdrAppearance {
        connector: connector.to_string(),
        appearance,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected HDR appearance response: {other:?}")),
        Err(err) => Err(err),
    }
}

fn set_live_hdr_calibration_pattern(
    connector: &str,
    pattern: HdrCalibrationPattern,
) -> Result<(), String> {
    match send_desktop_request(&IpcRequest::SetHdrCalibrationPattern {
        connector: connector.to_string(),
        pattern,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected HDR calibration response: {other:?}")),
        Err(err) => Err(err),
    }
}

fn display_preview_rect(
    d: &DisplayConfig,
    zoom: f64,
    offset_x: f64,
    offset_y: f64,
) -> (f64, f64, f64, f64) {
    let (width, height) = if matches!(d.transform.as_str(), "Rotate90" | "Rotate270") {
        (d.mode_height, d.mode_width)
    } else {
        (d.mode_width, d.mode_height)
    };
    let logical_w = width as f64 / d.scale.max(1.0);
    let logical_h = height as f64 / d.scale.max(1.0);

    (
        d.logical_x as f64 * zoom + offset_x,
        d.logical_y as f64 * zoom + offset_y,
        logical_w * zoom,
        logical_h * zoom,
    )
}

fn monitor_arrangement_area(displays: Rc<RefCell<Vec<DisplayConfig>>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(700);
    area.set_content_height(260);
    area.set_hexpand(true);

    let selected = Rc::new(RefCell::new(None::<usize>));
    let drag_start = Rc::new(RefCell::new((0, 0)));

    let zoom = 0.12;
    let offset_x = 40.0;
    let offset_y = 40.0;

    {
        let displays = displays.clone();

        area.set_draw_func(move |_, cr, _width, _height| {
            cr.set_source_rgb(0.08, 0.09, 0.11);
            let _ = cr.paint();

            let displays = displays.borrow();

            for d in displays.iter() {
                let (x, y, w, h) = display_preview_rect(d, zoom, offset_x, offset_y);

                cr.set_source_rgb(0.18, 0.22, 0.28);
                rounded_rect(cr, x, y, w, h, 14.0);
                let _ = cr.fill_preserve();

                if d.primary {
                    cr.set_source_rgb(0.75, 0.85, 1.0);
                    cr.set_line_width(3.0);
                } else {
                    cr.set_source_rgb(0.45, 0.60, 0.75);
                    cr.set_line_width(2.0);
                }

                let _ = cr.stroke();

                cr.set_source_rgb(0.95, 0.97, 1.0);
                cr.move_to(x + 14.0, y + 28.0);
                let _ = cr.show_text(&d.name);

                cr.move_to(x + 14.0, y + 50.0);
                let _ = cr.show_text(&format!(
                    "{}×{} @ {}Hz",
                    d.mode_width, d.mode_height, d.refresh_mhz
                ));
            }
        });
    }

    let drag = gtk::GestureDrag::new();

    {
        let displays = displays.clone();
        let selected = selected.clone();
        let drag_start = drag_start.clone();

        drag.connect_drag_begin(move |_, x, y| {
            let displays_ref = displays.borrow();

            let hit = displays_ref.iter().position(|d| {
                let (rx, ry, rw, rh) = display_preview_rect(d, zoom, offset_x, offset_y);
                x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
            });

            *selected.borrow_mut() = hit;

            if let Some(i) = hit {
                let d = &displays_ref[i];
                *drag_start.borrow_mut() = (d.logical_x, d.logical_y);
            }
        });
    }

    {
        let displays = displays.clone();
        let selected = selected.clone();
        let drag_start = drag_start.clone();
        let area_clone = area.clone();

        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(i) = *selected.borrow() {
                let (start_x, start_y) = *drag_start.borrow();

                let mut displays = displays.borrow_mut();
                displays[i].logical_x = start_x + (dx / zoom).round() as i32;
                displays[i].logical_y = start_y + (dy / zoom).round() as i32;

                area_clone.queue_draw();
            }
        });
    }

    {
        let displays = displays.clone();
        let selected = selected.clone();

        drag.connect_drag_end(move |_, _, _| {
            *selected.borrow_mut() = None;
            save_displays(&displays.borrow());
        });
    }

    area.add_controller(drag);
    area
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90f64.to_radians(), 0f64.to_radians());
    cr.arc(
        x + w - r,
        y + h - r,
        r,
        0f64.to_radians(),
        90f64.to_radians(),
    );
    cr.arc(x + r, y + h - r, r, 90f64.to_radians(), 180f64.to_radians());
    cr.arc(x + r, y + r, r, 180f64.to_radians(), 270f64.to_radians());
    cr.close_path();
}

fn displays_path() -> std::path::PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".config")
        })
        .join("focaldesk")
        .join("displays.json")
}

fn load_displays() -> Vec<DisplayConfig> {
    let path = displays_path();

    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut displays: Vec<DisplayConfig> = serde_json::from_str(&text).unwrap_or_default();
            for display in &mut displays {
                display.hdr_appearance = display.hdr_appearance.validate().unwrap_or_default();
            }
            displays
        }
        Err(_) => vec![],
    }
}

fn display_summary(d: &DisplayConfig) -> String {
    let profile = d
        .icc_profile_path
        .as_deref()
        .map(display_icc_profile_label)
        .unwrap_or_else(|| display_color_profile_label(d.color_profile));
    let gamut = if d.wide_gamut_active {
        "Wide-gamut active"
    } else {
        "sRGB advertised"
    };
    format!(
        "{}x{} @ {} Hz  |  {}  |  {}  |  {}  |  Scale {:.2}{}{}{}",
        d.mode_width,
        d.mode_height,
        d.refresh_mhz / 1000,
        transform_label(&d.transform),
        profile,
        gamut,
        d.scale,
        if d.primary { "  |  Primary" } else { "" },
        if d.enabled { "" } else { "  |  Disabled" },
        if d.icc_lut_fallback_active {
            "  |  ICC LUT fallback active"
        } else {
            ""
        }
    )
}

fn hdr_status_subtitle(hdr_requested: bool, hdr_enabled: bool) -> &'static str {
    if hdr_enabled {
        "Active now"
    } else if hdr_requested {
        "Requested, but inactive"
    } else {
        "Off"
    }
}

fn refresh_all_outputs_hdr_control(
    displays: &[DisplayConfig],
    row: &adw::ActionRow,
    button: &gtk::Button,
) {
    let active_capable = displays
        .iter()
        .filter(|display| display.enabled && display.hdr_supported)
        .collect::<Vec<_>>();
    let requested = active_capable
        .iter()
        .filter(|display| display.hdr_requested)
        .count();
    let subtitle = if active_capable.is_empty() {
        "No active HDR10-capable displays were detected".to_string()
    } else if requested == 0 {
        "Turn on HDR output request for one or more displays first".to_string()
    } else if requested < active_capable.len() {
        format!(
            "Apply HDR10 to all {} capable displays so they share the same encode; mixed HDR10/SDR will not match",
            active_capable.len()
        )
    } else {
        format!(
            "Apply HDR10 to {} capable display{}",
            active_capable.len(),
            if active_capable.len() == 1 { "" } else { "s" }
        )
    };
    row.set_subtitle(&subtitle);
    button.set_sensitive(requested > 0);
}

fn align_hdr_requests_across_capable_outputs(displays: &mut [DisplayConfig]) -> bool {
    let any_requested = displays
        .iter()
        .any(|display| display.enabled && display.hdr_supported && display.hdr_requested);
    if !any_requested {
        return false;
    }
    let mut changed = false;
    for display in displays.iter_mut() {
        if display.enabled && display.hdr_supported && !display.hdr_requested {
            display.hdr_requested = true;
            changed = true;
        }
    }
    changed
}

fn exclusive_hdr_status_text(phase: ExclusiveHdrPhase, reason: Option<&str>) -> String {
    match phase {
        ExclusiveHdrPhase::Off => {
            "Restarts into HDR10 on this display and disables every other monitor".to_string()
        }
        ExclusiveHdrPhase::Disabled => "Exclusive HDR10 is disabled".to_string(),
        ExclusiveHdrPhase::Requested => reason
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Exclusive HDR10 is armed for the next session".to_string()),
        ExclusiveHdrPhase::Starting => "Starting in exclusive SDR safety mode…".to_string(),
        ExclusiveHdrPhase::Verifying => {
            "Verifying HDR10 metadata and stable PQ scanout…".to_string()
        }
        ExclusiveHdrPhase::Active => {
            "HDR10 verified active; other monitors are disabled".to_string()
        }
        ExclusiveHdrPhase::Failed => format!(
            "Previous attempt failed: {}",
            reason.unwrap_or("unknown safety check failure")
        ),
    }
}

fn display_color_profile_label(profile: DisplayColorProfile) -> &'static str {
    match profile {
        DisplayColorProfile::Auto => "Auto color profile",
        DisplayColorProfile::Srgb => "sRGB output",
        DisplayColorProfile::DisplayP3 => "Display P3 output",
    }
}

fn display_icc_profile_label(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn transform_label(transform: &str) -> &'static str {
    match transform {
        "Rotate90" => "Portrait Right",
        "Rotate180" => "Landscape Flipped",
        "Rotate270" => "Portrait Left",
        _ => "Landscape",
    }
}

fn transform_index(transform: &str) -> u32 {
    match transform {
        "Rotate90" => 1,
        "Rotate180" => 2,
        "Rotate270" => 3,
        _ => 0,
    }
}

fn transform_from_index(index: u32) -> &'static str {
    match index {
        1 => "Rotate90",
        2 => "Rotate180",
        3 => "Rotate270",
        _ => "Normal",
    }
}

fn display_color_profile_index(profile: DisplayColorProfile) -> u32 {
    match profile {
        DisplayColorProfile::Auto => 0,
        DisplayColorProfile::Srgb => 1,
        DisplayColorProfile::DisplayP3 => 2,
    }
}

fn selected_display_color_profile(index: u32) -> DisplayColorProfile {
    match index {
        1 => DisplayColorProfile::Srgb,
        2 => DisplayColorProfile::DisplayP3,
        _ => DisplayColorProfile::Auto,
    }
}

fn resolution_options(display: &DisplayConfig) -> Vec<(i32, i32)> {
    let mut options = display
        .available_modes
        .iter()
        .map(|mode| (mode.width, mode.height))
        .collect::<Vec<_>>();

    if !options.contains(&(display.mode_width, display.mode_height)) {
        options.push((display.mode_width, display.mode_height));
    }

    options.sort_unstable();
    options.dedup();
    options
}

fn refresh_options(display: &DisplayConfig, width: i32, height: i32) -> Vec<i32> {
    let mut options = display
        .available_modes
        .iter()
        .filter(|mode| mode.width == width && mode.height == height)
        .map(|mode| mode.refresh_mhz)
        .collect::<Vec<_>>();
    if width == display.mode_width
        && height == display.mode_height
        && !options.contains(&display.refresh_mhz)
    {
        options.push(display.refresh_mhz);
    }
    options.sort_unstable();
    options.dedup();
    options
}

fn dropdown_from_strings(labels: &[&str], selected: u32) -> gtk::DropDown {
    let dropdown = gtk::DropDown::from_strings(labels);
    dropdown.set_selected(selected);
    dropdown
}

fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AudioDeviceKind {
    Sink,
    Source,
}

#[derive(Debug, Default)]
struct PactlAudioDevice {
    name: Option<String>,
    description: Option<String>,
    active_port: Option<String>,
}

#[derive(Debug, Clone)]
struct AudioDeviceChoice {
    selector: String,
    label: String,
}

fn normalize_audio_label(label: &str) -> String {
    label
        .trim()
        .trim_matches('"')
        .strip_prefix("alsa_input.")
        .or_else(|| label.trim().trim_matches('"').strip_prefix("alsa_output."))
        .unwrap_or_else(|| label.trim().trim_matches('"'))
        .replace(['_', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn audio_label_key(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

fn push_unique_audio_device(
    devices: &mut Vec<AudioDeviceChoice>,
    selector: String,
    label: String,
    prefer_first: bool,
) {
    let label = normalize_audio_label(&label);
    if selector.is_empty() || label.is_empty() {
        return;
    }

    if devices.iter().any(|known| known.selector == selector) {
        return;
    }

    let device = AudioDeviceChoice { selector, label };

    if prefer_first {
        devices.insert(0, device);
    } else {
        devices.push(device);
    }
}

fn pactl_value(line: &str, key: &str) -> Option<String> {
    line.trim()
        .strip_prefix(key)?
        .trim()
        .trim_matches('"')
        .trim()
        .to_string()
        .into()
}

fn pactl_port_label(line: &str, port_name: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix(port_name)?.strip_prefix(':')?.trim();
    let label = rest.split(" (").next().unwrap_or(rest).trim();
    (!label.is_empty()).then(|| label.to_string())
}

fn pactl_device_label(
    device: &PactlAudioDevice,
    ports: &HashMap<String, String>,
) -> Option<String> {
    let base = device
        .description
        .as_deref()
        .or(device.name.as_deref())
        .map(normalize_audio_label)?;
    if base.is_empty() {
        return None;
    }

    let Some(port_name) = device.active_port.as_deref() else {
        return Some(base);
    };
    let Some(port_label) = ports
        .get(port_name)
        .map(|label| normalize_audio_label(label))
    else {
        return Some(base);
    };
    if port_label.is_empty() || audio_label_key(&base).contains(&audio_label_key(&port_label)) {
        Some(base)
    } else {
        Some(format!("{base} - {port_label}"))
    }
}

fn push_pactl_device(
    current: &PactlAudioDevice,
    ports: &HashMap<String, String>,
    devices: &mut Vec<AudioDeviceChoice>,
    kind: AudioDeviceKind,
    default_name: Option<&str>,
) {
    if let Some(name) = current.name.as_deref() {
        if kind == AudioDeviceKind::Source && name.ends_with(".monitor") {
            return;
        }
    }

    if let Some(label) = pactl_device_label(current, ports) {
        if let Some(name) = current.name.as_deref() {
            let prefer_first = Some(name) == default_name;
            push_unique_audio_device(devices, name.to_string(), label, prefer_first);
        }
    }
}

fn parse_pactl_devices(
    output: &str,
    kind: AudioDeviceKind,
    default_name: Option<&str>,
) -> Vec<AudioDeviceChoice> {
    let mut devices = Vec::new();
    let mut current = PactlAudioDevice::default();
    let mut ports = HashMap::new();
    let mut in_ports = false;

    for line in output.lines() {
        let trimmed = line.trim();
        let is_header = match kind {
            AudioDeviceKind::Sink => trimmed.starts_with("Sink #"),
            AudioDeviceKind::Source => trimmed.starts_with("Source #"),
        };

        if is_header {
            if current.name.is_some() || current.description.is_some() {
                push_pactl_device(&current, &ports, &mut devices, kind, default_name);
                current = PactlAudioDevice::default();
                ports.clear();
                in_ports = false;
            }
            continue;
        }

        if let Some(name) = pactl_value(line, "Name:") {
            current.name = Some(name);
            continue;
        }

        if let Some(description) = pactl_value(line, "Description:") {
            current.description = Some(description);
            continue;
        }

        if trimmed == "Ports:" {
            in_ports = true;
            continue;
        }

        if let Some(active_port) = pactl_value(line, "Active Port:") {
            current.active_port = Some(active_port);
            in_ports = false;
            continue;
        }

        if in_ports {
            if !line.starts_with(char::is_whitespace) || trimmed.ends_with(':') {
                in_ports = false;
                continue;
            }

            if let Some((port_name, _)) = trimmed.split_once(':') {
                if let Some(label) = pactl_port_label(trimmed, port_name.trim()) {
                    ports.insert(port_name.trim().to_string(), label);
                }
            }
        }
    }

    if current.name.is_some() || current.description.is_some() {
        push_pactl_device(&current, &ports, &mut devices, kind, default_name);
    }

    devices
}

fn parse_pactl_short_devices(
    output: &str,
    kind: AudioDeviceKind,
    default_name: Option<&str>,
) -> Vec<AudioDeviceChoice> {
    let mut devices = Vec::new();

    for line in output.lines() {
        let mut fields = line.split('\t');
        fields.next();
        let Some(name) = fields.next().map(str::trim) else {
            continue;
        };

        if name.is_empty() || (kind == AudioDeviceKind::Source && name.ends_with(".monitor")) {
            continue;
        }

        push_unique_audio_device(
            &mut devices,
            name.to_string(),
            name.to_string(),
            Some(name) == default_name,
        );
    }

    devices
}

fn parse_wpctl_devices(output: &str, kind: AudioDeviceKind) -> Vec<AudioDeviceChoice> {
    let mut in_section = false;
    let mut devices = Vec::new();
    let section = match kind {
        AudioDeviceKind::Sink => "Sinks:",
        AudioDeviceKind::Source => "Sources:",
    };

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.contains(section) {
            in_section = true;
            continue;
        }

        if !in_section {
            continue;
        }

        if trimmed.ends_with(':') {
            break;
        }

        let Some((id, label)) = trimmed.split_once(". ") else {
            continue;
        };
        let label = label
            .split(" [")
            .next()
            .unwrap_or(label)
            .trim()
            .trim_start_matches('*')
            .trim();

        if label.is_empty() || label.to_ascii_lowercase().contains("monitor") {
            continue;
        }

        let prefer_first = trimmed.contains('*');
        let id = id.trim().trim_start_matches('*').trim();
        push_unique_audio_device(
            &mut devices,
            format!("wpctl:{id}"),
            label.to_string(),
            prefer_first,
        );
    }

    devices
}

fn parse_pactl_default_device(output: &str, kind: AudioDeviceKind) -> Option<String> {
    let key = match kind {
        AudioDeviceKind::Sink => "Default Sink:",
        AudioDeviceKind::Source => "Default Source:",
    };

    output.lines().find_map(|line| pactl_value(line, key))
}

fn load_audio_devices(kind: AudioDeviceKind) -> Result<Vec<AudioDeviceChoice>, String> {
    let list_arg = match kind {
        AudioDeviceKind::Sink => "sinks",
        AudioDeviceKind::Source => "sources",
    };

    match run_control_command("pactl", &["list", list_arg]) {
        Ok(output) => {
            let default_name = run_control_command("pactl", &["info"])
                .ok()
                .and_then(|output| parse_pactl_default_device(&output, kind));
            let devices = parse_pactl_devices(&output, kind, default_name.as_deref());
            if devices.is_empty() {
                run_control_command("pactl", &["list", "short", list_arg])
                    .map(|output| parse_pactl_short_devices(&output, kind, default_name.as_deref()))
            } else {
                Ok(devices)
            }
        }
        Err(pactl_err) => match run_control_command("wpctl", &["status"]) {
            Ok(output) => Ok(parse_wpctl_devices(&output, kind)),
            Err(wpctl_err) => Err(format!("{pactl_err}; {wpctl_err}")),
        },
    }
}

fn set_default_audio_device(kind: AudioDeviceKind, selector: &str) -> Result<(), String> {
    if let Some(id) = selector.strip_prefix("wpctl:") {
        return run_control_command("wpctl", &["set-default", id]).map(|_| ());
    }

    let command = match kind {
        AudioDeviceKind::Sink => "set-default-sink",
        AudioDeviceKind::Source => "set-default-source",
    };
    run_control_command("pactl", &[command, selector]).map(|_| ())
}

fn add_switch_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: Option<&str>,
    active: bool,
) -> gtk::Switch {
    let row = adw::ActionRow::new();
    row.set_title(title);
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }

    let switch = gtk::Switch::new();
    switch.set_active(active);
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    group.add(&row);

    switch
}

fn add_dropdown_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: Option<&str>,
    labels: &[&str],
    selected: u32,
) -> gtk::DropDown {
    let row = adw::ActionRow::new();
    row.set_title(title);
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }

    let dropdown = dropdown_from_strings(labels, selected);
    row.add_suffix(&dropdown);
    group.add(&row);

    dropdown
}

fn add_info_row(group: &adw::PreferencesGroup, title: &str, subtitle: Option<&str>, value: &str) {
    let row = adw::ActionRow::new();
    row.set_title(title);
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row.add_suffix(&dim_label(value));
    group.add(&row);
}

fn add_button_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: Option<&str>,
    label: &str,
) -> gtk::Button {
    let row = adw::ActionRow::new();
    row.set_title(title);
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }

    let button = gtk::Button::with_label(label);
    button.add_css_class("pill");
    row.add_suffix(&button);
    group.add(&row);

    button
}

fn add_entry_row(group: &adw::PreferencesGroup, title: &str, placeholder: &str) -> gtk::Entry {
    let row = adw::ActionRow::new();
    row.set_title(title);

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    row.add_suffix(&entry);
    group.add(&row);

    entry
}

fn add_scale_row(
    group: &adw::PreferencesGroup,
    title: &str,
    min: f64,
    max: f64,
    step: f64,
    value: f64,
) -> gtk::Scale {
    let row = adw::ActionRow::new();
    row.set_title(title);

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_hexpand(true);
    scale.set_draw_value(true);
    scale.set_value(value);
    row.add_suffix(&scale);
    group.add(&row);

    scale
}

fn suffix_chevron() -> gtk::Image {
    gtk::Image::from_icon_name("go-next-symbolic")
}

fn run_control_command(program: &str, args: &[&str]) -> Result<String, String> {
    run_control_command_with_timeout(program, args, Duration::from_secs(2))
}

fn run_control_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("{program}: {err}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program}: command timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("{program}: {err}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|err| format!("{program}: {err}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
        Err(format!("{program}: {message}"))
    }
}

fn split_nmcli_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut escape = false;

    for ch in line.chars() {
        if escape {
            current.push(ch);
            escape = false;
        } else if ch == '\\' {
            escape = true;
        } else if ch == ':' {
            fields.push(current);
            current = String::new();
        } else {
            current.push(ch);
        }
    }

    fields.push(current);
    fields
}

fn load_wifi_snapshot() -> WifiSnapshot {
    let enabled = match run_control_command("nmcli", &["-t", "-f", "WIFI", "radio"]) {
        Ok(output) => output.lines().next().unwrap_or_default().trim() == "enabled",
        Err(err) => {
            return WifiSnapshot {
                enabled: false,
                networks: vec![],
                error: Some(err),
            };
        }
    };

    let list = match run_control_command_with_timeout(
        "nmcli",
        &[
            "-t",
            "-f",
            "IN-USE,SSID,SECURITY,SIGNAL",
            "device",
            "wifi",
            "list",
            "--rescan",
            "yes",
        ],
        Duration::from_secs(15),
    )
    .or_else(|_| {
        run_control_command_with_timeout(
            "nmcli",
            &[
                "-t",
                "-f",
                "IN-USE,SSID,SECURITY,SIGNAL",
                "device",
                "wifi",
                "list",
                "--rescan",
                "no",
            ],
            Duration::from_secs(5),
        )
    }) {
        Ok(output) => output,
        Err(err) => {
            return WifiSnapshot {
                enabled,
                networks: vec![],
                error: Some(err),
            };
        }
    };

    let mut networks = Vec::new();

    for line in list.lines() {
        let fields = split_nmcli_line(line);
        let ssid = fields.get(1).map(String::as_str).unwrap_or_default().trim();
        if ssid.is_empty() {
            continue;
        }

        let security = fields
            .get(2)
            .map(String::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let signal = fields
            .get(3)
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0);

        networks.push(WifiNetwork {
            active: fields.first().map(String::as_str).unwrap_or_default() == "*",
            ssid: ssid.to_string(),
            security,
            signal,
        });
    }

    networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    networks.dedup_by(|a, b| a.ssid == b.ssid);

    WifiSnapshot {
        enabled,
        networks,
        error: None,
    }
}

fn load_ethernet_snapshot() -> EthernetSnapshot {
    let output = match run_control_command(
        "nmcli",
        &[
            "-t",
            "-f",
            "DEVICE,TYPE,STATE,CONNECTION",
            "device",
            "status",
        ],
    ) {
        Ok(output) => output,
        Err(err) => {
            return EthernetSnapshot {
                devices: vec![],
                error: Some(err),
            };
        }
    };

    let mut devices = Vec::new();

    for line in output.lines() {
        let fields = split_nmcli_line(line);
        if fields.get(1).map(String::as_str) != Some("ethernet") {
            continue;
        }

        let device = fields.first().cloned().unwrap_or_default();
        if device.is_empty() {
            continue;
        }

        let connection = fields
            .get(3)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty() && *value != "--")
            .map(str::to_string);

        devices.push(EthernetDevice {
            device,
            state: fields
                .get(2)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            connection,
        });
    }

    devices.sort_by(|a, b| {
        ethernet_connected(b)
            .cmp(&ethernet_connected(a))
            .then(a.device.cmp(&b.device))
    });

    EthernetSnapshot {
        devices,
        error: None,
    }
}

fn parse_default_printer(output: &str) -> Option<String> {
    output
        .strip_prefix("system default destination:")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn parse_printer_devices(output: &str) -> HashMap<String, String> {
    let mut devices = HashMap::new();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("device for ") {
            if let Some((name, uri)) = rest.split_once(':') {
                devices.insert(name.trim().to_string(), uri.trim().to_string());
            }
        }
    }

    devices
}

fn sanitize_printer_name(name: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_separator = false;

    for ch in name.chars() {
        let ch = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            ch
        } else if ch.is_whitespace() || matches!(ch, ':' | '/' | '%' | '?' | '&' | '=') {
            '_'
        } else {
            continue;
        };

        let is_separator = matches!(ch, '_' | '-' | '.');
        if is_separator && last_was_separator {
            continue;
        }

        sanitized.push(ch);
        last_was_separator = is_separator;
    }

    let sanitized = sanitized.trim_matches(['_', '-', '.']).to_string();
    if sanitized.is_empty() {
        "Printer".to_string()
    } else {
        sanitized
    }
}

fn suggested_printer_name(uri: &str) -> String {
    let decoded = uri.replace("%20", " ");
    let without_query = decoded.split('?').next().unwrap_or(decoded.as_str());
    let candidate = without_query
        .rsplit_once('/')
        .map(|(_, tail)| tail)
        .filter(|tail| !tail.is_empty())
        .or_else(|| {
            without_query
                .split_once("://")
                .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        })
        .unwrap_or("Printer");

    sanitize_printer_name(candidate)
}

fn parse_installable_printers(
    output: &str,
    configured_devices: &HashMap<String, String>,
) -> Vec<InstallablePrinter> {
    let configured_uris: Vec<&str> = configured_devices.values().map(String::as_str).collect();
    let mut installable = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        let Some((kind, uri)) = line.split_once(' ') else {
            continue;
        };
        let kind = kind.trim();
        let uri = uri.trim();

        if kind.is_empty() || uri.is_empty() || configured_uris.contains(&uri) {
            continue;
        }

        installable.push(InstallablePrinter {
            kind: kind.to_string(),
            uri: uri.to_string(),
            suggested_name: suggested_printer_name(uri),
        });
    }

    installable.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.suggested_name.cmp(&b.suggested_name))
            .then(a.uri.cmp(&b.uri))
    });
    installable.dedup_by(|a, b| a.uri == b.uri);
    installable
}

fn parse_printer_states(output: &str, devices: &HashMap<String, String>) -> Vec<Printer> {
    let mut printers = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with("printer ") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let _printer_word = parts.next();
        let Some(name) = parts.next() else {
            continue;
        };

        let enabled = !line.contains(" disabled ");
        let state = line
            .split_once(" is ")
            .map(|(_, rest)| rest.split('.').next().unwrap_or(rest).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        printers.push(Printer {
            name: name.to_string(),
            enabled,
            accepting_jobs: true,
            is_default: false,
            state,
            device_uri: devices.get(name).cloned(),
        });
    }

    printers
}

fn apply_accepting_jobs(printers: &mut [Printer], output: &str) {
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with("printer ") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let _printer_word = parts.next();
        let Some(name) = parts.next() else {
            continue;
        };

        if let Some(printer) = printers.iter_mut().find(|printer| printer.name == name) {
            printer.accepting_jobs = line.contains(" accepting requests ");
        }
    }
}

fn valid_printer_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn install_printer(name: &str, uri: &str, model: &str) -> Result<String, String> {
    let name = name.trim();
    let uri = uri.trim();
    let model = model.trim();
    let model = if model.is_empty() {
        "everywhere"
    } else {
        model
    };

    if !valid_printer_name(name) {
        return Err(
            "Printer name can only contain letters, numbers, dots, hyphens, and underscores"
                .to_string(),
        );
    }

    if uri.is_empty() {
        return Err("Device URI is required".to_string());
    }

    run_control_command_with_timeout(
        "lpadmin",
        &["-p", name, "-E", "-v", uri, "-m", model],
        Duration::from_secs(10),
    )
}

fn load_printer_snapshot() -> PrinterSnapshot {
    let scheduler_running = match run_control_command("lpstat", &["-r"]) {
        Ok(output) => output.contains("scheduler is running"),
        Err(err) => {
            return PrinterSnapshot {
                scheduler_running: false,
                printers: vec![],
                installable_printers: vec![],
                error: Some(err),
            };
        }
    };

    let devices_output = run_control_command("lpstat", &["-v"]).unwrap_or_default();
    let devices = parse_printer_devices(&devices_output);
    let installable_printers =
        run_control_command_with_timeout("lpinfo", &["-v"], Duration::from_secs(8))
            .map(|output| parse_installable_printers(&output, &devices))
            .unwrap_or_default();

    let printers_output = match run_control_command("lpstat", &["-p"]) {
        Ok(output) => output,
        Err(err) => {
            return PrinterSnapshot {
                scheduler_running,
                printers: vec![],
                installable_printers,
                error: Some(err),
            };
        }
    };

    let mut printers = parse_printer_states(&printers_output, &devices);

    if let Ok(accepting_output) = run_control_command("lpstat", &["-a"]) {
        apply_accepting_jobs(&mut printers, &accepting_output);
    }

    if let Ok(default_output) = run_control_command("lpstat", &["-d"]) {
        if let Some(default_name) = parse_default_printer(&default_output) {
            for printer in &mut printers {
                printer.is_default = printer.name == default_name;
            }
        }
    }

    printers.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.enabled.cmp(&a.enabled))
            .then(a.name.cmp(&b.name))
    });

    PrinterSnapshot {
        scheduler_running,
        printers,
        installable_printers,
        error: None,
    }
}

fn save_display_change(
    displays: &Rc<RefCell<Vec<DisplayConfig>>>,
    area: &gtk::DrawingArea,
    row: &adw::ExpanderRow,
    index: usize,
) {
    let displays_ref = displays.borrow();
    save_displays(&displays_ref);
    area.queue_draw();

    if let Some(display) = displays_ref.get(index) {
        row.set_subtitle(&display_summary(display));
    }
}

#[derive(Clone, Copy)]
enum HdrAppearanceField {
    BlackLevel,
    ReferenceWhite,
    Peak,
    FullFramePeak,
    Saturation,
    MidtoneGamma,
}

fn hdr_appearance_preset(selected: u32) -> Option<HdrAppearance> {
    match selected {
        0 => Some(HdrAppearance::default()),
        1 => Some(HdrAppearance {
            black_level_nits: 0.05,
            reference_white_nits: 250.0,
            peak_nits: 450.0,
            full_frame_peak_nits: 300.0,
            saturation: 1.0,
            midtone_gamma: 0.90,
        }),
        2 => Some(HdrAppearance {
            black_level_nits: 0.05,
            reference_white_nits: 203.0,
            peak_nits: 450.0,
            full_frame_peak_nits: 300.0,
            saturation: 1.10,
            midtone_gamma: 1.10,
        }),
        _ => None,
    }
}

fn hdr_appearance_preset_index(appearance: HdrAppearance) -> u32 {
    (0..HDR_APPEARANCE_CUSTOM_PRESET)
        .find(|selected| hdr_appearance_preset(*selected) == Some(appearance))
        .unwrap_or(HDR_APPEARANCE_CUSTOM_PRESET)
}

fn hdr_calibration_pattern(selected: u32) -> HdrCalibrationPattern {
    match selected {
        1 => HdrCalibrationPattern::Overview,
        2 => HdrCalibrationPattern::NearBlack,
        3 => HdrCalibrationPattern::ReferenceWhite,
        4 => HdrCalibrationPattern::PeakWindow,
        5 => HdrCalibrationPattern::PeakFullFrame,
        _ => HdrCalibrationPattern::Off,
    }
}

fn hdr_tuning_scale(min: f64, max: f64, step: f64, value: f32, digits: i32) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_value(f64::from(value));
    scale.set_digits(digits);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    scale.set_size_request(300, -1);
    scale
}

#[allow(clippy::too_many_arguments)]
fn start_hdr_appearance_preview(
    connector: &str,
    appearance: HdrAppearance,
    confirmed: Rc<RefCell<HdrAppearance>>,
    draft: Rc<RefCell<HdrAppearance>>,
    generation: Rc<Cell<u64>>,
    suppress: Rc<Cell<bool>>,
    status: adw::ActionRow,
    black_level: gtk::Scale,
    reference_white: gtk::Scale,
    peak: gtk::Scale,
    full_frame_peak: gtk::Scale,
    saturation: gtk::Scale,
    midtone_gamma: gtk::Scale,
    preset: gtk::DropDown,
) {
    if let Err(message) = appearance.validate() {
        status.set_subtitle(&format!("Not previewed: {message}"));
        return;
    }
    if let Err(message) = set_live_hdr_appearance(connector, appearance) {
        status.set_subtitle(&format!("Preview unavailable: {message}"));
        return;
    }

    let preview_generation = generation.get().wrapping_add(1);
    generation.set(preview_generation);
    status.set_subtitle("Previewing live for 15 seconds; choose Keep to save");

    let connector = connector.to_string();
    glib::timeout_add_local_once(Duration::from_secs(15), move || {
        if generation.get() != preview_generation {
            return;
        }
        let rollback = *confirmed.borrow();
        if let Err(message) = set_live_hdr_appearance(&connector, rollback) {
            status.set_subtitle(&format!(
                "Could not restore saved HDR appearance: {message}"
            ));
            return;
        }
        suppress.set(true);
        *draft.borrow_mut() = rollback;
        preset.set_selected(hdr_appearance_preset_index(rollback));
        black_level.set_value(f64::from(rollback.black_level_nits));
        reference_white.set_value(f64::from(rollback.reference_white_nits));
        peak.set_value(f64::from(rollback.peak_nits));
        full_frame_peak.set_value(f64::from(rollback.full_frame_peak_nits));
        saturation.set_value(f64::from(rollback.saturation));
        midtone_gamma.set_value(f64::from(rollback.midtone_gamma));
        suppress.set(false);
        generation.set(preview_generation.wrapping_add(1));
        status.set_subtitle("Preview expired; restored the saved values");
    });
}

#[allow(clippy::too_many_arguments)]
fn connect_hdr_appearance_scale(
    scale: &gtk::Scale,
    field: HdrAppearanceField,
    connector: String,
    confirmed: Rc<RefCell<HdrAppearance>>,
    draft: Rc<RefCell<HdrAppearance>>,
    generation: Rc<Cell<u64>>,
    suppress: Rc<Cell<bool>>,
    status: adw::ActionRow,
    black_level: gtk::Scale,
    reference_white: gtk::Scale,
    peak: gtk::Scale,
    full_frame_peak: gtk::Scale,
    saturation: gtk::Scale,
    midtone_gamma: gtk::Scale,
    preset: gtk::DropDown,
) {
    scale.connect_value_changed(move |scale| {
        if suppress.get() {
            return;
        }
        let mut appearance = *draft.borrow();
        let value = scale.value() as f32;
        match field {
            HdrAppearanceField::BlackLevel => appearance.black_level_nits = value,
            HdrAppearanceField::ReferenceWhite => {
                appearance.reference_white_nits = value;
                if appearance.peak_nits < value {
                    appearance.peak_nits = value;
                    suppress.set(true);
                    peak.set_value(f64::from(value));
                    suppress.set(false);
                }
            }
            HdrAppearanceField::Peak => {
                appearance.peak_nits = value;
                if appearance.reference_white_nits > value {
                    appearance.reference_white_nits = value;
                    suppress.set(true);
                    reference_white.set_value(f64::from(value));
                    suppress.set(false);
                }
                if appearance.full_frame_peak_nits > value {
                    appearance.full_frame_peak_nits = value;
                    suppress.set(true);
                    full_frame_peak.set_value(f64::from(value));
                    suppress.set(false);
                }
            }
            HdrAppearanceField::FullFramePeak => {
                appearance.full_frame_peak_nits = value;
                if appearance.peak_nits < value {
                    appearance.peak_nits = value;
                    suppress.set(true);
                    peak.set_value(f64::from(value));
                    suppress.set(false);
                }
            }
            HdrAppearanceField::Saturation => appearance.saturation = value,
            HdrAppearanceField::MidtoneGamma => appearance.midtone_gamma = value,
        }
        *draft.borrow_mut() = appearance;
        suppress.set(true);
        preset.set_selected(HDR_APPEARANCE_CUSTOM_PRESET);
        suppress.set(false);
        start_hdr_appearance_preview(
            &connector,
            appearance,
            confirmed.clone(),
            draft.clone(),
            generation.clone(),
            suppress.clone(),
            status.clone(),
            black_level.clone(),
            reference_white.clone(),
            peak.clone(),
            full_frame_peak.clone(),
            saturation.clone(),
            midtone_gamma.clone(),
            preset.clone(),
        );
    });
}

fn hdr_appearance_row(index: usize, displays: Rc<RefCell<Vec<DisplayConfig>>>) -> adw::ExpanderRow {
    let display = displays.borrow()[index].clone();
    let connector = display.name.clone();
    let initial = display.hdr_appearance.validate().unwrap_or_default();
    let confirmed = Rc::new(RefCell::new(initial));
    let draft = Rc::new(RefCell::new(initial));
    let generation = Rc::new(Cell::new(0u64));
    let suppress = Rc::new(Cell::new(false));

    let section = adw::ExpanderRow::new();
    section.set_title("HDR appearance tuning");
    section.set_subtitle("Final PQ shader only; does not change HDR signaling or display modes");

    let black_level = hdr_tuning_scale(0.0, 0.25, 0.005, initial.black_level_nits, 3);
    let reference_white = hdr_tuning_scale(80.0, 450.0, 1.0, initial.reference_white_nits, 0);
    let peak = hdr_tuning_scale(203.0, 450.0, 1.0, initial.peak_nits, 0);
    let full_frame_peak = hdr_tuning_scale(80.0, 450.0, 1.0, initial.full_frame_peak_nits, 0);
    let saturation = hdr_tuning_scale(0.75, 1.25, 0.01, initial.saturation, 2);
    let midtone_gamma = hdr_tuning_scale(0.70, 1.50, 0.01, initial.midtone_gamma, 2);
    let preset = gtk::DropDown::from_strings(HDR_APPEARANCE_PRESET_OPTIONS);
    preset.set_selected(hdr_appearance_preset_index(initial));

    let preset_row = adw::ActionRow::new();
    preset_row.set_title("Preset");
    preset_row.set_subtitle("Conservative starting points; selecting one starts a safe preview");
    preset_row.add_suffix(&preset);
    section.add_row(&preset_row);

    let calibration_pattern = gtk::DropDown::from_strings(HDR_CALIBRATION_PATTERN_OPTIONS);
    calibration_pattern.set_selected(0);
    let calibration_row = adw::ActionRow::new();
    calibration_row.set_title("Calibration pattern");
    calibration_row.set_subtitle(
        "Session only; move Settings to another display before selecting a full-screen target",
    );
    calibration_row.add_suffix(&calibration_pattern);
    section.add_row(&calibration_row);

    for (title, subtitle, control) in [
        (
            "Black level",
            "Set until the second near-black band is barely distinguishable from signal black",
            &black_level,
        ),
        (
            "Reference white",
            "Diffuse desktop white in nits; neutral default is 203",
            &reference_white,
        ),
        (
            "Peak luminance",
            "10% highlight target in nits; use the centered peak window",
            &peak,
        ),
        (
            "Full-frame peak",
            "Sustained full-screen white in nits; exported as MaxFALL",
            &full_frame_peak,
        ),
        (
            "Saturation",
            "Luminance-preserving panel-gamut adjustment; 1.00 is neutral",
            &saturation,
        ),
        (
            "Midtone gamma",
            "Shapes SDR-range midtones while preserving black and white; 1.00 is neutral",
            &midtone_gamma,
        ),
    ] {
        let row = adw::ActionRow::new();
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.add_suffix(control);
        section.add_row(&row);
    }

    let status = adw::ActionRow::new();
    status.set_title("Preview safety");
    status.set_subtitle("Changes restore automatically after 15 seconds unless kept");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let reset = gtk::Button::with_label("Reset");
    let keep = gtk::Button::with_label("Keep");
    keep.add_css_class("suggested-action");
    buttons.append(&reset);
    buttons.append(&keep);
    status.add_suffix(&buttons);
    section.add_row(&status);

    {
        let connector = connector.clone();
        let status = status.clone();
        calibration_pattern.connect_selected_notify(move |selector| {
            let pattern = hdr_calibration_pattern(selector.selected());
            match set_live_hdr_calibration_pattern(&connector, pattern) {
                Ok(()) if pattern == HdrCalibrationPattern::Off => {
                    status.set_subtitle("Calibration pattern disabled");
                }
                Ok(()) => {
                    status.set_subtitle(
                        "Calibration target active on the HDR display; select Off when finished",
                    );
                }
                Err(message) => status.set_subtitle(&format!("Calibration unavailable: {message}")),
            }
        });
    }

    for (scale, field) in [
        (&black_level, HdrAppearanceField::BlackLevel),
        (&reference_white, HdrAppearanceField::ReferenceWhite),
        (&peak, HdrAppearanceField::Peak),
        (&full_frame_peak, HdrAppearanceField::FullFramePeak),
        (&saturation, HdrAppearanceField::Saturation),
        (&midtone_gamma, HdrAppearanceField::MidtoneGamma),
    ] {
        connect_hdr_appearance_scale(
            scale,
            field,
            connector.clone(),
            confirmed.clone(),
            draft.clone(),
            generation.clone(),
            suppress.clone(),
            status.clone(),
            black_level.clone(),
            reference_white.clone(),
            peak.clone(),
            full_frame_peak.clone(),
            saturation.clone(),
            midtone_gamma.clone(),
            preset.clone(),
        );
    }

    {
        let connector = connector.clone();
        let confirmed = confirmed.clone();
        let draft = draft.clone();
        let generation = generation.clone();
        let suppress = suppress.clone();
        let status = status.clone();
        let black_level = black_level.clone();
        let reference_white = reference_white.clone();
        let peak = peak.clone();
        let full_frame_peak = full_frame_peak.clone();
        let saturation = saturation.clone();
        let midtone_gamma = midtone_gamma.clone();
        preset.connect_selected_notify(move |preset| {
            if suppress.get() {
                return;
            }
            let Some(appearance) = hdr_appearance_preset(preset.selected()) else {
                return;
            };
            suppress.set(true);
            *draft.borrow_mut() = appearance;
            black_level.set_value(f64::from(appearance.black_level_nits));
            reference_white.set_value(f64::from(appearance.reference_white_nits));
            peak.set_value(f64::from(appearance.peak_nits));
            full_frame_peak.set_value(f64::from(appearance.full_frame_peak_nits));
            saturation.set_value(f64::from(appearance.saturation));
            midtone_gamma.set_value(f64::from(appearance.midtone_gamma));
            suppress.set(false);
            start_hdr_appearance_preview(
                &connector,
                appearance,
                confirmed.clone(),
                draft.clone(),
                generation.clone(),
                suppress.clone(),
                status.clone(),
                black_level.clone(),
                reference_white.clone(),
                peak.clone(),
                full_frame_peak.clone(),
                saturation.clone(),
                midtone_gamma.clone(),
                preset.clone(),
            );
        });
    }

    {
        let connector = connector.clone();
        let confirmed = confirmed.clone();
        let draft = draft.clone();
        let generation = generation.clone();
        let suppress = suppress.clone();
        let status = status.clone();
        let black_level = black_level.clone();
        let reference_white = reference_white.clone();
        let peak = peak.clone();
        let full_frame_peak = full_frame_peak.clone();
        let saturation = saturation.clone();
        let midtone_gamma = midtone_gamma.clone();
        let preset = preset.clone();
        reset.connect_clicked(move |_| {
            let defaults = HdrAppearance::default();
            suppress.set(true);
            *draft.borrow_mut() = defaults;
            preset.set_selected(0);
            black_level.set_value(f64::from(defaults.black_level_nits));
            reference_white.set_value(f64::from(defaults.reference_white_nits));
            peak.set_value(f64::from(defaults.peak_nits));
            full_frame_peak.set_value(f64::from(defaults.full_frame_peak_nits));
            saturation.set_value(f64::from(defaults.saturation));
            midtone_gamma.set_value(f64::from(defaults.midtone_gamma));
            suppress.set(false);
            start_hdr_appearance_preview(
                &connector,
                defaults,
                confirmed.clone(),
                draft.clone(),
                generation.clone(),
                suppress.clone(),
                status.clone(),
                black_level.clone(),
                reference_white.clone(),
                peak.clone(),
                full_frame_peak.clone(),
                saturation.clone(),
                midtone_gamma.clone(),
                preset.clone(),
            );
        });
    }

    {
        let displays = displays.clone();
        let confirmed = confirmed.clone();
        let draft = draft.clone();
        let generation = generation.clone();
        let status = status.clone();
        keep.connect_clicked(move |_| {
            let appearance = *draft.borrow();
            if let Err(message) = appearance.validate() {
                status.set_subtitle(&format!("Cannot save: {message}"));
                return;
            }
            generation.set(generation.get().wrapping_add(1));
            *confirmed.borrow_mut() = appearance;
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.hdr_appearance = appearance;
            }
            persist_displays(&displays.borrow());
            status.set_subtitle("Saved for this display");
        });
    }

    section
}

fn connected_display_row(
    index: usize,
    displays: Rc<RefCell<Vec<DisplayConfig>>>,
    area: gtk::DrawingArea,
    row_registry: Rc<RefCell<HashMap<String, adw::ExpanderRow>>>,
    hdr_switch_registry: Rc<RefCell<HashMap<String, gtk::Switch>>>,
    bulk_hdr_update: Rc<Cell<bool>>,
    hdr_requests_dirty: Rc<Cell<bool>>,
    all_hdr_row: adw::ActionRow,
    all_hdr_button: gtk::Button,
    parent: adw::ApplicationWindow,
) -> adw::ExpanderRow {
    let display = displays.borrow()[index].clone();
    let row = adw::ExpanderRow::new();
    row.set_title(&display.name);
    row.set_subtitle(&display_summary(&display));
    row.set_enable_expansion(true);
    row_registry
        .borrow_mut()
        .insert(display.name.clone(), row.clone());

    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_tooltip_text(Some("Display details"));
    row.add_suffix(&info);

    let chevron = gtk::Image::from_icon_name("go-next-symbolic");
    chevron.set_tooltip_text(Some("Expand display settings"));
    row.add_suffix(&chevron);

    let resolution_row = adw::ActionRow::new();
    resolution_row.set_title("Resolution");
    let resolutions = resolution_options(&display);
    let resolution_labels = resolutions
        .iter()
        .map(|(width, height)| format!("{width} x {height}"))
        .collect::<Vec<_>>();
    let resolution_label_refs = resolution_labels
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let resolution_dropdown = dropdown_from_strings(
        &resolution_label_refs,
        resolutions
            .iter()
            .position(|resolution| *resolution == (display.mode_width, display.mode_height))
            .unwrap_or(0) as u32,
    );
    resolution_row.add_suffix(&resolution_dropdown);
    row.add_row(&resolution_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        let available_modes = display.available_modes.clone();
        resolution_dropdown.connect_selected_notify(move |dropdown| {
            let Some((width, height)) = resolutions.get(dropdown.selected() as usize).copied()
            else {
                return;
            };

            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.mode_width = width;
                display.mode_height = height;
                if !available_modes.iter().any(|mode| {
                    mode.width == width
                        && mode.height == height
                        && mode.refresh_mhz == display.refresh_mhz
                }) {
                    if let Some(refresh_mhz) = available_modes
                        .iter()
                        .filter(|mode| mode.width == width && mode.height == height)
                        .map(|mode| mode.refresh_mhz)
                        .max()
                    {
                        display.refresh_mhz = refresh_mhz;
                    }
                }
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let refresh_row = adw::ActionRow::new();
    refresh_row.set_title("Refresh rate");
    refresh_row.set_subtitle("Takes effect after signing out and back in");
    let refresh_rates = refresh_options(&display, display.mode_width, display.mode_height);
    let refresh_labels = refresh_rates
        .iter()
        .map(|refresh_mhz| format!("{} Hz", refresh_mhz / 1000))
        .collect::<Vec<_>>();
    let refresh_label_refs = refresh_labels
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let refresh_dropdown = dropdown_from_strings(
        &refresh_label_refs,
        refresh_rates
            .iter()
            .position(|refresh_mhz| *refresh_mhz == display.refresh_mhz)
            .unwrap_or(0) as u32,
    );
    refresh_row.add_suffix(&refresh_dropdown);
    row.add_row(&refresh_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        refresh_dropdown.connect_selected_notify(move |dropdown| {
            let Some(refresh_mhz) = refresh_rates.get(dropdown.selected() as usize).copied() else {
                return;
            };
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.refresh_mhz = refresh_mhz;
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let orientation_row = adw::ActionRow::new();
    orientation_row.set_title("Orientation");
    let orientation_dropdown =
        dropdown_from_strings(ORIENTATION_OPTIONS, transform_index(&display.transform));
    orientation_row.add_suffix(&orientation_dropdown);
    row.add_row(&orientation_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        orientation_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.transform = transform_from_index(dropdown.selected()).to_string();
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let scale_row = adw::ActionRow::new();
    scale_row.set_title("Scale");
    let scale_dropdown = gtk::DropDown::from_strings(
        &SCALE_OPTIONS
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>(),
    );
    scale_dropdown.set_selected(
        SCALE_OPTIONS
            .iter()
            .position(|(_, value)| (*value - display.scale).abs() < 0.01)
            .unwrap_or(0) as u32,
    );
    scale_row.add_suffix(&scale_dropdown);
    row.add_row(&scale_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        scale_dropdown.connect_selected_notify(move |dropdown| {
            let selected = dropdown.selected() as usize;
            if let (Some(display), Some((_, scale))) = (
                displays.borrow_mut().get_mut(index),
                SCALE_OPTIONS.get(selected),
            ) {
                display.scale = *scale;
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let primary_row = adw::ActionRow::new();
    primary_row.set_title("Primary Display");
    let primary = gtk::Switch::new();
    primary.set_active(display.primary);
    primary_row.add_suffix(&primary);
    primary_row.set_activatable_widget(Some(&primary));
    row.add_row(&primary_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        primary.connect_active_notify(move |switch| {
            let active = switch.is_active();
            let mut displays_ref = displays.borrow_mut();
            if active {
                for display in displays_ref.iter_mut() {
                    display.primary = false;
                }
            }
            if let Some(display) = displays_ref.get_mut(index) {
                display.primary = active;
            }
            drop(displays_ref);
            save_display_change(&displays, &area, &row, index);
        });
    }

    let enabled_row = adw::ActionRow::new();
    enabled_row.set_title("Enabled");
    let enabled = gtk::Switch::new();
    enabled.set_active(display.enabled);
    enabled_row.add_suffix(&enabled);
    enabled_row.set_activatable_widget(Some(&enabled));
    row.add_row(&enabled_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        enabled.connect_active_notify(move |switch| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.enabled = switch.is_active();
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    if display.hdr_supported {
        let hdr_row = adw::ActionRow::new();
        hdr_row.set_title("HDR output request");
        hdr_row.set_subtitle(hdr_status_subtitle(
            display.hdr_requested,
            display.hdr_enabled,
        ));
        let hdr = gtk::Switch::new();
        hdr.set_active(display.hdr_requested || display.hdr_enabled);
        hdr_switch_registry
            .borrow_mut()
            .insert(display.name.clone(), hdr.clone());
        hdr_row.add_suffix(&hdr);
        hdr_row.set_activatable_widget(Some(&hdr));
        row.add_row(&hdr_row);

        {
            let displays = displays.clone();
            let area = area.clone();
            let row = row.clone();
            let hdr_row = hdr_row.clone();
            let bulk_hdr_update = bulk_hdr_update.clone();
            let hdr_requests_dirty = hdr_requests_dirty.clone();
            let all_hdr_row = all_hdr_row.clone();
            let all_hdr_button = all_hdr_button.clone();
            hdr.connect_active_notify(move |switch| {
                if bulk_hdr_update.get() {
                    return;
                }
                if let Some(display) = displays.borrow_mut().get_mut(index) {
                    display.hdr_requested = display.hdr_supported && switch.is_active();
                    if !display.hdr_requested {
                        display.hdr_enabled = false;
                    }
                    hdr_row.set_subtitle(hdr_status_subtitle(
                        display.hdr_requested,
                        display.hdr_enabled,
                    ));
                }
                persist_displays(&displays.borrow());
                area.queue_draw();
                if let Some(display) = displays.borrow().get(index) {
                    row.set_subtitle(&display_summary(display));
                }
                hdr_requests_dirty.set(true);
                refresh_all_outputs_hdr_control(&displays.borrow(), &all_hdr_row, &all_hdr_button);
            });
        }

        row.add_row(&hdr_appearance_row(index, displays.clone()));

        let exclusive_state = load_exclusive_hdr_state();
        let exclusive_phase = (exclusive_state.connector.as_deref() == Some(display.name.as_str()))
            .then_some(exclusive_state.phase)
            .unwrap_or_default();
        let exclusive_row = adw::ActionRow::new();
        exclusive_row.set_title("Experimental exclusive HDR10");
        exclusive_row.set_subtitle(&exclusive_hdr_status_text(
            exclusive_phase,
            exclusive_state.reason.as_deref(),
        ));
        let exclusive_button = gtk::Button::with_label(match exclusive_phase {
            ExclusiveHdrPhase::Active => "Disable & Restart",
            ExclusiveHdrPhase::Requested if exclusive_state.reason.is_some() => "Log Out Manually",
            ExclusiveHdrPhase::Requested | ExclusiveHdrPhase::Starting => "Restarting…",
            ExclusiveHdrPhase::Verifying => "Verifying…",
            _ => "Restart & Try HDR10",
        });
        exclusive_button.add_css_class("destructive-action");
        exclusive_button.set_valign(gtk::Align::Center);
        exclusive_button.set_sensitive(!matches!(
            exclusive_phase,
            ExclusiveHdrPhase::Requested
                | ExclusiveHdrPhase::Starting
                | ExclusiveHdrPhase::Verifying
        ));
        exclusive_row.add_suffix(&exclusive_button);
        row.add_row(&exclusive_row);

        {
            let connector = display.name.clone();
            let exclusive_row = exclusive_row.clone();
            let exclusive_button = exclusive_button.clone();
            exclusive_button.clone().connect_clicked(move |_| {
                let current = load_exclusive_hdr_state();
                let disabling = current.connector.as_deref() == Some(connector.as_str())
                    && matches!(current.phase, ExclusiveHdrPhase::Active);
                let next = if disabling {
                    ExclusiveHdrState {
                        phase: ExclusiveHdrPhase::Disabled,
                        connector: Some(connector.clone()),
                        reason: None,
                        session_id: None,
                    }
                } else {
                    ExclusiveHdrState {
                        phase: ExclusiveHdrPhase::Requested,
                        connector: Some(connector.clone()),
                        reason: None,
                        session_id: None,
                    }
                };
                match save_exclusive_hdr_state(&next) {
                    Ok(()) => {
                        exclusive_row.set_subtitle(if disabling {
                            "Normal multi-monitor SDR is armed for the next session"
                        } else {
                            "Restarting into guarded exclusive HDR10 verification…"
                        });
                        exclusive_button.set_sensitive(false);
                        let response = send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                            action: DesktopAction::Logout,
                        });
                        if !matches!(response, Ok(IpcResponse::Ok)) {
                            let reason = format!(
                                "HDR10 is armed, but automatic restart was unavailable ({response:?}). Log out once manually to load the updated compositor."
                            );
                            let mut awaiting_logout = next.clone();
                            awaiting_logout.reason = Some(reason.clone());
                            if let Err(err) = save_exclusive_hdr_state(&awaiting_logout) {
                                exclusive_row.set_subtitle(&format!(
                                    "{reason} Could not save restart status: {err}"
                                ));
                            } else {
                                exclusive_row.set_subtitle(&reason);
                                exclusive_button.set_label("Log Out Manually");
                            }
                        }
                    }
                    Err(err) => {
                        exclusive_row
                            .set_subtitle(&format!("Could not arm exclusive HDR10: {err}"));
                    }
                }
            });
        }

        {
            let connector = display.name.clone();
            let exclusive_row = exclusive_row.clone();
            let exclusive_button = exclusive_button.clone();
            let hdr_row = hdr_row.clone();
            glib::timeout_add_local(Duration::from_millis(250), move || {
                let state = load_exclusive_hdr_state();
                let phase = (state.connector.as_deref() == Some(connector.as_str()))
                    .then_some(state.phase)
                    .unwrap_or_default();
                exclusive_row
                    .set_subtitle(&exclusive_hdr_status_text(phase, state.reason.as_deref()));
                exclusive_button.set_label(match phase {
                    ExclusiveHdrPhase::Active => "Disable & Restart",
                    ExclusiveHdrPhase::Requested if state.reason.is_some() => "Log Out Manually",
                    ExclusiveHdrPhase::Requested | ExclusiveHdrPhase::Starting => "Restarting…",
                    ExclusiveHdrPhase::Verifying => "Verifying…",
                    _ => "Restart & Try HDR10",
                });
                exclusive_button.set_sensitive(!matches!(
                    phase,
                    ExclusiveHdrPhase::Requested
                        | ExclusiveHdrPhase::Starting
                        | ExclusiveHdrPhase::Verifying
                ));
                match phase {
                    ExclusiveHdrPhase::Requested | ExclusiveHdrPhase::Starting => {
                        hdr_row.set_subtitle("Starting exclusive HDR10…")
                    }
                    ExclusiveHdrPhase::Verifying => {
                        hdr_row.set_subtitle("Verifying HDR10 stability…")
                    }
                    ExclusiveHdrPhase::Active => hdr_row.set_subtitle("Active now"),
                    ExclusiveHdrPhase::Failed => hdr_row.set_subtitle("Failed; restored safe SDR"),
                    ExclusiveHdrPhase::Off | ExclusiveHdrPhase::Disabled => {}
                }
                glib::ControlFlow::Continue
            });
        }
    }

    let color_row = adw::ActionRow::new();
    color_row.set_title("Color gamut");
    color_row
        .set_subtitle("Auto uses the monitor profile; sRGB and Display P3 override SDR color only");
    let color_dropdown = dropdown_from_strings(
        DISPLAY_COLOR_PROFILE_OPTIONS,
        display_color_profile_index(display.color_profile),
    );
    color_row.add_suffix(&color_dropdown);
    color_row.set_activatable_widget(Some(&color_dropdown));
    row.add_row(&color_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        color_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.color_profile = selected_display_color_profile(dropdown.selected());
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let icc_row = adw::ActionRow::new();
    icc_row.set_title("ICC profile file");
    let icc_subtitle = display
        .icc_profile_path
        .as_deref()
        .map(|path| format!("Selected: {}", display_icc_profile_label(path)))
        .unwrap_or_else(|| "No ICC file selected".to_string());
    icc_row.set_subtitle(&icc_subtitle);
    let icc_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let choose_icc = gtk::Button::with_label("Choose ICC...");
    let clear_icc = gtk::Button::with_label("Clear");
    icc_buttons.append(&choose_icc);
    icc_buttons.append(&clear_icc);
    icc_row.add_suffix(&icc_buttons);
    icc_row.set_activatable_widget(Some(&choose_icc));
    row.add_row(&icc_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        let parent = parent.clone();
        let icc_row = icc_row.clone();
        choose_icc.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Choose ICC Profile");
            dialog.open(Some(&parent), None::<&gtk::gio::Cancellable>, {
                let displays = displays.clone();
                let area = area.clone();
                let row = row.clone();
                let icc_row = icc_row.clone();
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            if let Some(display) = displays.borrow_mut().get_mut(index) {
                                display.icc_profile_path =
                                    Some(path.to_string_lossy().into_owned());
                            }
                            if let Some(display) = displays.borrow().get(index) {
                                let subtitle = display
                                    .icc_profile_path
                                    .as_deref()
                                    .map(|p| format!("Selected: {}", display_icc_profile_label(p)))
                                    .unwrap_or_else(|| "No ICC file selected".to_string());
                                icc_row.set_subtitle(&subtitle);
                            }
                            save_display_change(&displays, &area, &row, index);
                        }
                    }
                }
            });
        });
    }

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        let icc_row = icc_row.clone();
        clear_icc.connect_clicked(move |_| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.icc_profile_path = None;
            }
            icc_row.set_subtitle("No ICC file selected");
            save_display_change(&displays, &area, &row, index);
        });
    }

    row
}

fn persist_config(config: &FocalDeskConfig) {
    if let Err(err) = send_desktop_config(config.clone()) {
        info!(
            target: "focaldesk",
            session_id = session_id(),
            error = %err,
            "settings IPC unavailable; saving config directly"
        );
        let _ = save_config(config);
    }
}

fn persist_config_key(config: &FocalDeskConfig, key: &str, value: serde_json::Value) {
    if let Err(err) = send_desktop_set(key, value) {
        warn!(
            target: "focaldesk",
            session_id = session_id(),
            key = %key,
            error = %err,
            "settings IPC set failed; saving config directly"
        );
        let _ = save_config(config);
    }
}

fn persist_settings(settings: &Settings) {
    if let Err(err) = save_settings(settings) {
        error!(
            target: "focaldesk",
            session_id = session_id(),
            error = %err,
            "failed to save settings"
        );
        return;
    }

    if let Err(err) = send_settings_request(&IpcRequest::Reload) {
        info!(
            target: "focaldesk",
            session_id = session_id(),
            error = %err,
            "settings IPC reload unavailable after settings save"
        );
    }
}

fn focaldesk_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
}

fn focaldesk_config_path() -> PathBuf {
    focaldesk_config_dir().join("config.toml")
}

fn focaldesk_log_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(path) = std::env::var("FOCALDESK_LOG_FILE") {
        paths.push(PathBuf::from(path));
    }

    if let Some(state_dir) = dirs::state_dir() {
        paths.push(state_dir.join("focaldesk").join("focaldesk.log"));
    }

    if let Some(cache_dir) = dirs::cache_dir() {
        paths.push(cache_dir.join("focaldesk").join("focaldesk.log"));
    }

    paths.push(PathBuf::from("/tmp/focaldesk.log"));
    paths
}

fn existing_focaldesk_log_path() -> Option<PathBuf> {
    focaldesk_log_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

fn open_path(path: &PathBuf) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("xdg-open failed: {err}"))
}

fn session_type_label() -> String {
    std::env::var("XDG_SESSION_TYPE")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|_| "wayland".to_string())
        })
        .or_else(|| {
            std::env::var("DISPLAY")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|_| "x11".to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn diagnostics_text() -> String {
    let settings_path = focaldesk_settings_core::settings_path();
    let log_path = existing_focaldesk_log_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "not found".to_string());
    let current_exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    format!(
        "FocalDesk diagnostics\n\
         Version: {}\n\
         Build hash: {}\n\
         Build profile: {}\n\
         OS/arch: {}/{}\n\
         Session type: {}\n\
         Current executable: {}\n\
         Config path: {}\n\
         Settings path: {}\n\
         Log path: {}\n\
         WAYLAND_DISPLAY: {}\n\
         DISPLAY: {}\n\
         XDG_CURRENT_DESKTOP: {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("VERGEN_GIT_SHA").unwrap_or("development"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
        session_type_label(),
        current_exe,
        focaldesk_config_path().display(),
        settings_path.display(),
        log_path,
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".to_string()),
        std::env::var("DISPLAY").unwrap_or_else(|_| "unset".to_string()),
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unset".to_string()),
    )
}

fn start_config_watch(keys: &[&str]) -> mpsc::Receiver<ConfigEvent> {
    let (tx, rx) = mpsc::channel();
    let keys = keys.iter().map(|key| (*key).to_string()).collect();

    thread::spawn(move || {
        if let Err(err) = watch_desktop_keys(keys, move |response| match response {
            IpcResponse::Event { key, value } => {
                let _ = tx.send(ConfigEvent { key, value });
            }
            IpcResponse::Error { message } => {
                warn!(
                    target: "focaldesk",
                    session_id = session_id(),
                    message = %message,
                    "settings IPC watch error"
                );
            }
            _ => {}
        }) {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "settings IPC watch unavailable"
            );
        }
    });

    rx
}

fn load_display_runtime_statuses() -> Vec<DisplayRuntimeOutputStatus> {
    match send_desktop_request(&IpcRequest::GetDisplayRuntimeStatus) {
        Ok(IpcResponse::DisplayRuntimeStatus { outputs }) => outputs,
        Ok(IpcResponse::Error { message }) => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                message = %message,
                "display runtime status query rejected"
            );
            Vec::new()
        }
        Ok(other) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                response = ?other,
                "unexpected display runtime status response"
            );
            Vec::new()
        }
        Err(err) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "display runtime status unavailable"
            );
            Vec::new()
        }
    }
}

fn apply_runtime_statuses(
    displays: &Rc<RefCell<Vec<DisplayConfig>>>,
    statuses: &[DisplayRuntimeOutputStatus],
    preserve_staged_hdr_requests: bool,
) {
    let mut displays_ref = displays.borrow_mut();
    for display in displays_ref.iter_mut() {
        let status = statuses
            .iter()
            .find(|status| status.connector == display.name)
            .cloned();
        let fallback_active = status
            .as_ref()
            .map(|status| status.icc_lut_fallback_active)
            .unwrap_or(false);
        let wide_gamut_active = status
            .as_ref()
            .map(|status| status.wide_gamut_active)
            .unwrap_or(false);
        let hdr_supported = status
            .as_ref()
            .map(|status| status.hdr_supported)
            .unwrap_or(display.hdr_supported);
        let hdr_requested = status
            .as_ref()
            .map(|status| status.hdr_requested)
            .unwrap_or(display.hdr_requested);
        let hdr_enabled = status
            .as_ref()
            .map(|status| status.hdr_active)
            .unwrap_or(false);
        let exclusive_hdr_phase = status
            .as_ref()
            .map(|status| status.exclusive_hdr_phase)
            .unwrap_or_default();
        let exclusive_hdr_reason = status
            .as_ref()
            .and_then(|status| status.exclusive_hdr_reason.clone());

        if display.icc_lut_fallback_active != fallback_active {
            display.icc_lut_fallback_active = fallback_active;
        }
        if display.wide_gamut_active != wide_gamut_active {
            display.wide_gamut_active = wide_gamut_active;
        }
        display.hdr_supported = hdr_supported;
        if !preserve_staged_hdr_requests {
            display.hdr_requested = hdr_requested;
        }
        display.hdr_enabled = hdr_enabled;
        display.exclusive_hdr_phase = exclusive_hdr_phase;
        display.exclusive_hdr_reason = exclusive_hdr_reason;
    }
}

fn set_switch_if_changed(switch: &gtk::Switch, active: bool) {
    if switch.is_active() != active {
        switch.set_active(active);
    }
}

fn set_scale_if_changed(scale: &gtk::Scale, value: f64) {
    if (scale.value() - value).abs() > f64::EPSILON {
        scale.set_value(value);
    }
}

fn dock_visibility_index(visibility: DockVisibility) -> u32 {
    match visibility {
        DockVisibility::IntelligentDodge => 0,
        DockVisibility::AlwaysVisible => 1,
        DockVisibility::Autohide => 2,
    }
}

fn dock_visibility_from_index(index: u32) -> DockVisibility {
    match index {
        1 => DockVisibility::AlwaysVisible,
        2 => DockVisibility::Autohide,
        _ => DockVisibility::IntelligentDodge,
    }
}

fn main() {
    init_default_logging();
    let initial_panel = requested_panel(std::env::args().skip(1));
    let app = adw::Application::new(
        Some("com.focaldesk.Settings"),
        gtk::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.connect_activate(move |app| build_ui(app, initial_panel.as_deref()));
    app.run();
}

fn install_focaldesk_theme() {
    let provider = gtk::CssProvider::new();
    let initial = active_theme_snapshot();
    apply_theme_snapshot(&provider, &initial);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let current = Rc::new(RefCell::new(initial));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let next = active_theme_snapshot();
        if next != *current.borrow() {
            apply_theme_snapshot(&provider, &next);
            *current.borrow_mut() = next;
        }
        glib::ControlFlow::Continue
    });
}

fn active_theme_snapshot() -> (String, GtkAppThemeOptions) {
    let config = load_config();
    let settings = load_settings();
    (
        config.appearance.theme,
        GtkAppThemeOptions {
            font_scale: config.appearance.font_scale,
            animations: settings.appearance.animations,
            high_contrast: settings.appearance.high_contrast,
        },
    )
}

fn apply_theme_snapshot(provider: &gtk::CssProvider, snapshot: &(String, GtkAppThemeOptions)) {
    let theme = theme_by_name(&snapshot.0);
    adw::StyleManager::default().set_color_scheme(if gtk_app_prefers_dark(&theme) {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    provider.load_from_string(&gtk_app_css(&theme, snapshot.1));
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(snapshot.1.animations);
    }
}

fn requested_panel(mut args: impl Iterator<Item = String>) -> Option<String> {
    while let Some(argument) = args.next() {
        if argument == "--panel" {
            return args.next().map(|panel| panel.to_ascii_lowercase());
        }
        if let Some(panel) = argument.strip_prefix("--panel=") {
            return Some(panel.to_ascii_lowercase());
        }
    }
    None
}

fn build_ui(app: &adw::Application, initial_panel: Option<&str>) {
    install_focaldesk_theme();
    let config = Rc::new(RefCell::new(load_config()));
    let settings = Rc::new(RefCell::new(load_settings()));

    let window = adw::ApplicationWindow::new(app);
    window.add_css_class("focaldesk-app");
    window.set_title(Some("FocalDesk Settings"));
    window.set_default_size(1000, 700);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let split = adw::NavigationSplitView::new();

    // ----- sidebar -----
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.add_css_class("settings-sidebar");
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);

    let panel_names = [
        "Appearance",
        "Theme Editor",
        "Network",
        "Bluetooth",
        "Printers",
        "Displays",
        "Sound",
        "Applications",
        "Chrome",
        "Workspaces",
        "Keyboard",
        "Privacy",
        "Power",
        "Debug",
        "About",
    ];
    for name in panel_names {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_margin_top(10);
        label.set_margin_bottom(10);
        label.set_margin_start(12);
        label.set_margin_end(12);

        row.set_child(Some(&label));
        list.append(&row);
    }

    sidebar_box.append(&list);

    let sidebar_page = adw::NavigationPage::new(&sidebar_box, "Settings");

    // ----- content pages -----
    let content_stack = gtk::Stack::new();
    content_stack.set_hexpand(true);
    content_stack.set_vexpand(true);
    let content_page = adw::NavigationPage::new(&content_stack, "Content");
    let mut pages: HashMap<String, adw::NavigationPage> = HashMap::new();

    pages.insert(
        "Appearance".to_string(),
        appearance_page(config.clone(), settings.clone()),
    );
    pages.insert("Theme Editor".to_string(), theme_editor_page());
    pages.insert("Network".to_string(), network_page());
    pages.insert("Bluetooth".to_string(), bluetooth_page());
    pages.insert("Printers".to_string(), printers_page());
    pages.insert(
        "Displays".to_string(),
        displays_page(config.clone(), window.clone()),
    );
    pages.insert("Sound".to_string(), sound_page());
    pages.insert(
        "Applications".to_string(),
        applications_page(settings.clone()),
    );
    pages.insert("Chrome".to_string(), chrome_page(settings.clone()));
    pages.insert("Workspaces".to_string(), workspaces_page(settings.clone()));
    pages.insert("Keyboard".to_string(), keyboard_page(settings.clone()));
    pages.insert("Privacy".to_string(), privacy_page(settings.clone()));
    pages.insert("Power".to_string(), power_page(settings.clone()));
    pages.insert("Debug".to_string(), debug_page(settings.clone()));
    pages.insert("About".to_string(), about_page());

    for (name, page) in &pages {
        content_stack.add_named(page, Some(name.as_str()));
    }

    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    let initial_index = initial_panel
        .and_then(|requested| {
            panel_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(requested))
        })
        .unwrap_or(0);
    content_stack.set_visible_child_name(panel_names[initial_index]);
    list.select_row(list.row_at_index(initial_index as i32).as_ref());

    let content_stack_clone = content_stack.clone();

    list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            if let Some(label) = row.child().and_then(|w| w.downcast::<gtk::Label>().ok()) {
                let text = label.text().to_string();
                content_stack_clone.set_visible_child_name(&text);
            }
        }
    });

    toolbar.set_content(Some(&split));
    window.set_content(Some(&toolbar));
    window.present();
}

#[derive(Debug, Clone)]
struct ThemeEditorColor {
    space: ThemeColorSpace,
    cached_srgb: Option<ThemeColor>,
    cached_display_p3: Option<ThemeColor>,
}

impl ThemeEditorColor {
    fn new(color: ThemeColor) -> Self {
        Self {
            space: color.space,
            cached_srgb: (color.space == ThemeColorSpace::Srgb).then_some(color),
            cached_display_p3: (color.space == ThemeColorSpace::DisplayP3).then_some(color),
        }
    }

    fn color(&self) -> ThemeColor {
        match self.space {
            ThemeColorSpace::Srgb => self.cached_srgb.expect("active sRGB color must be cached"),
            ThemeColorSpace::DisplayP3 => self
                .cached_display_p3
                .expect("active Display P3 color must be cached"),
            ThemeColorSpace::Rec2020 => unreachable!("Rec.2020 is not exposed in Phase 6"),
        }
    }

    fn store(&mut self, color: ThemeColor) {
        self.space = color.space;
        match color.space {
            ThemeColorSpace::Srgb => self.cached_srgb = Some(color),
            ThemeColorSpace::DisplayP3 => self.cached_display_p3 = Some(color),
            ThemeColorSpace::Rec2020 => unreachable!("Rec.2020 is not exposed in Phase 6"),
        }
    }

    fn switch_space(&mut self, target: ThemeColorSpace) -> ThemeColor {
        let current = self.color();
        let target_color = match target {
            ThemeColorSpace::Srgb => self
                .cached_srgb
                .unwrap_or_else(|| current.converted_to(ThemeColorSpace::Srgb)),
            ThemeColorSpace::DisplayP3 => self
                .cached_display_p3
                .unwrap_or_else(|| current.converted_to(ThemeColorSpace::DisplayP3)),
            ThemeColorSpace::Rec2020 => unreachable!("Rec.2020 is not exposed in Phase 6"),
        };
        self.store(target_color);
        target_color
    }
}

#[derive(Debug, Clone)]
struct ThemeEditorStop {
    position: f32,
    color: ThemeEditorColor,
}

#[derive(Debug, Clone)]
struct ThemeEditorDraft {
    theme_name: String,
    wallpaper: ThemeWallpaper,
    semantic: SemanticTheme,
    mode: u32,
    hue: f64,
    saturation: f64,
    value: f64,
    alpha: f64,
    active_color: ThemeEditorColor,
    solid_color: ThemeEditorColor,
    stops: Vec<ThemeEditorStop>,
    selected_stop: usize,
    interpolation_space: ThemeColorSpace,
    linear_angle: f64,
    radial_center: (f64, f64),
    radial_radius: f64,
    dynamic_range: ThemeDynamicRange,
    hdr_luminance_nits: f64,
}

impl ThemeEditorDraft {
    fn new(hue: f64, saturation: f64, value: f64, alpha: f64) -> Self {
        let [r, g, b] = hsv_to_rgb(hue, saturation, value);
        let srgb = ThemeColor::srgb(
            srgb_decode(r) as f32,
            srgb_decode(g) as f32,
            srgb_decode(b) as f32,
            alpha as f32,
        );
        let solid_color = ThemeEditorColor::new(srgb);
        let second = ThemeColor::srgb(0.035, 0.012, 0.180, 1.0);
        Self {
            theme_name: "Untitled Theme".to_string(),
            wallpaper: ThemeWallpaper::default(),
            semantic: SemanticTheme::default(),
            mode: 0,
            hue,
            saturation,
            value,
            alpha,
            active_color: solid_color.clone(),
            solid_color: solid_color.clone(),
            stops: vec![
                ThemeEditorStop {
                    position: 0.0,
                    color: solid_color,
                },
                ThemeEditorStop {
                    position: 1.0,
                    color: ThemeEditorColor::new(second),
                },
            ],
            selected_stop: 0,
            interpolation_space: ThemeColorSpace::Srgb,
            linear_angle: 135.0,
            radial_center: (0.5, 0.5),
            radial_radius: 0.75,
            dynamic_range: ThemeDynamicRange::Sdr,
            hdr_luminance_nits: 1_000.0,
        }
    }

    fn space(&self) -> ThemeColorSpace {
        self.active_color.space
    }

    fn current_color(&self) -> ThemeColor {
        let [r, g, b] = hsv_to_rgb(self.hue, self.saturation, self.value);
        ThemeColor::new(
            self.space(),
            srgb_decode(r) as f32,
            srgb_decode(g) as f32,
            srgb_decode(b) as f32,
            self.alpha as f32,
        )
    }

    fn commit_current_color(&mut self) {
        let color = self.current_color();
        self.active_color.store(color);
        if self.mode == 0 {
            self.solid_color = self.active_color.clone();
        } else if let Some(stop) = self.stops.get_mut(self.selected_stop) {
            stop.color = self.active_color.clone();
        }
    }

    fn switch_space(&mut self, target: ThemeColorSpace) {
        if self.space() == target {
            return;
        }
        self.commit_current_color();
        let mut active = self.active_color.clone();
        let color = active.switch_space(target);
        self.active_color = active;
        self.load_picker_color(color);
        self.commit_current_color();
    }

    fn load_picker_color(&mut self, color: ThemeColor) {
        let (hue, saturation, value) = rgb_to_hsv([
            srgb_encode(color.r).clamp(0.0, 1.0),
            srgb_encode(color.g).clamp(0.0, 1.0),
            srgb_encode(color.b).clamp(0.0, 1.0),
        ]);
        self.hue = hue;
        self.saturation = saturation;
        self.value = value;
        self.alpha = f64::from(color.a);
    }

    fn switch_mode(&mut self, mode: u32) {
        if self.mode == mode {
            return;
        }
        self.commit_current_color();
        self.mode = mode.min(2);
        let active = if self.mode == 0 {
            self.solid_color.clone()
        } else {
            self.stops[self.selected_stop.min(self.stops.len() - 1)]
                .color
                .clone()
        };
        let color = active.color();
        self.active_color = active;
        self.load_picker_color(color);
    }

    fn select_stop(&mut self, selected: usize) {
        if self.mode == 0 || self.stops.is_empty() {
            return;
        }
        self.commit_current_color();
        self.selected_stop = selected.min(self.stops.len() - 1);
        let active = self.stops[self.selected_stop].color.clone();
        let color = active.color();
        self.active_color = active;
        self.load_picker_color(color);
    }

    fn add_stop(&mut self) {
        self.commit_current_color();
        let position = if self.stops.len() < 2 {
            0.5
        } else {
            let mut positions: Vec<f32> = self.stops.iter().map(|stop| stop.position).collect();
            positions.sort_by(f32::total_cmp);
            positions
                .windows(2)
                .max_by(|left, right| (left[1] - left[0]).total_cmp(&(right[1] - right[0])))
                .map(|pair| (pair[0] + pair[1]) * 0.5)
                .unwrap_or(0.5)
        };
        self.stops.push(ThemeEditorStop {
            position,
            color: self.active_color.clone(),
        });
        self.selected_stop = self.stops.len() - 1;
    }

    fn duplicate_stop(&mut self) {
        self.commit_current_color();
        let selected = self.selected_stop.min(self.stops.len() - 1);
        let mut duplicate = self.stops[selected].clone();
        duplicate.position = (duplicate.position + 0.05).min(1.0);
        self.stops.push(duplicate);
        self.selected_stop = self.stops.len() - 1;
    }

    fn remove_stop(&mut self) {
        if self.stops.len() <= 2 {
            return;
        }
        self.stops
            .remove(self.selected_stop.min(self.stops.len() - 1));
        self.selected_stop = self.selected_stop.min(self.stops.len() - 1);
        let active = self.stops[self.selected_stop].color.clone();
        let color = active.color();
        self.active_color = active;
        self.load_picker_color(color);
    }

    fn set_selected_stop_position(&mut self, position: f32) {
        if let Some(stop) = self.stops.get_mut(self.selected_stop) {
            stop.position = position.clamp(0.0, 1.0);
        }
    }

    fn paint(&self) -> ThemePaint {
        if self.mode == 0 {
            return ThemePaint::solid(self.current_color());
        }
        let mut stops: Vec<GradientStop> = self
            .stops
            .iter()
            .enumerate()
            .map(|(index, stop)| GradientStop {
                position: stop.position,
                color: if index == self.selected_stop {
                    self.current_color()
                } else {
                    stop.color.color()
                },
            })
            .collect();
        stops.sort_by(|left, right| left.position.total_cmp(&right.position));
        let interpolation = GradientInterpolation {
            space: self.interpolation_space,
            premultiplied_alpha: true,
        };
        if self.mode == 1 {
            ThemePaint::LinearGradient {
                angle: self.linear_angle as f32,
                interpolation,
                stops,
            }
        } else {
            ThemePaint::RadialGradient {
                center: (self.radial_center.0 as f32, self.radial_center.1 as f32),
                radius: self.radial_radius as f32,
                interpolation,
                stops,
            }
        }
    }

    fn paint_intent(&self) -> ThemePaintIntent {
        ThemePaintIntent {
            paint: self.paint(),
            dynamic_range: self.dynamic_range,
            hdr_luminance_nits: self.hdr_luminance_nits as f32,
        }
    }

    fn document(&self) -> ThemeDocument {
        let mut document = ThemeDocument::new(self.theme_name.clone(), self.paint_intent());
        document.wallpaper = self.wallpaper.clone();
        document.semantic = self.semantic.clone();
        document
    }

    fn from_document(document: &ThemeDocument) -> Result<Self, String> {
        let supported_color = |color: ThemeColor| {
            if color.space == ThemeColorSpace::Rec2020 {
                return Err("Rec.2020 themes are not editable in Phase 7".to_string());
            }
            if ![color.r, color.g, color.b]
                .into_iter()
                .all(|component| (0.0..=1.0).contains(&component))
            {
                return Err("theme color is outside the editable RGB cube".to_string());
            }
            Ok(ThemeEditorColor::new(color))
        };

        document.validate().map_err(|error| error.to_string())?;
        let mut draft = Self::new(0.0, 0.0, 0.0, 1.0);
        draft.theme_name = document.name.clone();
        draft.wallpaper = document.wallpaper.clone();
        draft.semantic = document.semantic.clone();
        draft.dynamic_range = document.intent.dynamic_range;
        draft.hdr_luminance_nits = f64::from(document.intent.hdr_luminance_nits);

        match &document.intent.paint {
            ThemePaint::Solid { color } => {
                let color = supported_color(*color)?;
                draft.mode = 0;
                draft.active_color = color.clone();
                draft.solid_color = color;
            }
            ThemePaint::LinearGradient {
                angle,
                interpolation,
                stops,
            } => {
                if interpolation.space == ThemeColorSpace::Rec2020 {
                    return Err("Rec.2020 interpolation is not editable in Phase 7".to_string());
                }
                if !interpolation.premultiplied_alpha {
                    return Err(
                        "straight-alpha gradient interpolation is not editable in Phase 7"
                            .to_string(),
                    );
                }
                if !(0.0..=360.0).contains(angle) {
                    return Err("linear gradient angle must be between 0 and 360".to_string());
                }
                draft.mode = 1;
                draft.linear_angle = f64::from(*angle);
                draft.interpolation_space = interpolation.space;
                draft.stops = stops
                    .iter()
                    .map(|stop| {
                        Ok(ThemeEditorStop {
                            position: stop.position,
                            color: supported_color(stop.color)?,
                        })
                    })
                    .collect::<Result<_, String>>()?;
                draft.selected_stop = 0;
                draft.active_color = draft.stops[0].color.clone();
            }
            ThemePaint::RadialGradient {
                center,
                radius,
                interpolation,
                stops,
            } => {
                if interpolation.space == ThemeColorSpace::Rec2020 {
                    return Err("Rec.2020 interpolation is not editable in Phase 7".to_string());
                }
                if !interpolation.premultiplied_alpha {
                    return Err(
                        "straight-alpha gradient interpolation is not editable in Phase 7"
                            .to_string(),
                    );
                }
                if !(0.0..=1.0).contains(&center.0)
                    || !(0.0..=1.0).contains(&center.1)
                    || !(0.01..=2.0).contains(radius)
                {
                    return Err("radial gradient geometry is outside editor limits".to_string());
                }
                draft.mode = 2;
                draft.radial_center = (f64::from(center.0), f64::from(center.1));
                draft.radial_radius = f64::from(*radius);
                draft.interpolation_space = interpolation.space;
                draft.stops = stops
                    .iter()
                    .map(|stop| {
                        Ok(ThemeEditorStop {
                            position: stop.position,
                            color: supported_color(stop.color)?,
                        })
                    })
                    .collect::<Result<_, String>>()?;
                draft.selected_stop = 0;
                draft.active_color = draft.stops[0].color.clone();
            }
        }
        let color = draft.active_color.color();
        draft.load_picker_color(color);
        Ok(draft)
    }

    fn preview_paint(&self) -> ThemePaint {
        self.paint_intent().mapped_for_sdr_preview()
    }

    fn preview_color(&self, color: ThemeColor) -> ThemeColor {
        let intent = ThemePaintIntent {
            paint: ThemePaint::solid(color),
            dynamic_range: self.dynamic_range,
            hdr_luminance_nits: self.hdr_luminance_nits as f32,
        };
        let ThemePaint::Solid { color } = intent.mapped_for_sdr_preview() else {
            unreachable!()
        };
        color
    }

    fn preview_current_color(&self) -> ThemeColor {
        self.preview_color(self.current_color())
    }
}

fn srgb_decode(component: f64) -> f64 {
    if component <= 0.04045 {
        component / 12.92
    } else {
        ((component + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(component: f32) -> f64 {
    let component = f64::from(component.max(0.0));
    if component <= 0.003_130_8 {
        component * 12.92
    } else {
        1.055 * component.powf(1.0 / 2.4) - 0.055
    }
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> [f64; 3] {
    let chroma = value * saturation;
    let sector = (hue.rem_euclid(360.0)) / 60.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let [r, g, b] = match sector as u32 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    let match_value = value - chroma;
    [r + match_value, g + match_value, b + match_value]
}

fn rgb_to_hsv([r, g, b]: [f64; 3]) -> (f64, f64, f64) {
    let maximum = r.max(g).max(b);
    let minimum = r.min(g).min(b);
    let delta = maximum - minimum;
    let hue = if delta <= f64::EPSILON {
        0.0
    } else if maximum == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if maximum == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if maximum <= f64::EPSILON {
        0.0
    } else {
        delta / maximum
    };
    (hue, saturation, maximum)
}

fn wrap_hue(hue: f64) -> f64 {
    hue.rem_euclid(360.0)
}

fn hue_from_ring_point(width: f64, height: f64, x: f64, y: f64) -> f64 {
    let center_x = width / 2.0;
    let center_y = height / 2.0;
    wrap_hue((y - center_y).atan2(x - center_x).to_degrees())
}

fn point_is_on_hue_ring(width: f64, height: f64, x: f64, y: f64) -> bool {
    let center_x = width / 2.0;
    let center_y = height / 2.0;
    let ring_radius = width.min(height) / 2.0 - 18.0;
    let distance = ((x - center_x).powi(2) + (y - center_y).powi(2)).sqrt();
    (distance - ring_radius).abs() <= 16.0
}

fn saturation_value_from_point(width: f64, height: f64, x: f64, y: f64) -> (f64, f64) {
    let square_size = (width.min(height) - 16.0).max(1.0);
    let square_x = (width - square_size) / 2.0;
    let square_y = (height - square_size) / 2.0;
    (
        ((x - square_x) / square_size).clamp(0.0, 1.0),
        (1.0 - (y - square_y) / square_size).clamp(0.0, 1.0),
    )
}

fn picker_color(space: ThemeColorSpace, hue: f64, saturation: f64, value: f64) -> ThemeColor {
    let [r, g, b] = hsv_to_rgb(hue, saturation, value);
    ThemeColor::new(
        space,
        srgb_decode(r) as f32,
        srgb_decode(g) as f32,
        srgb_decode(b) as f32,
        1.0,
    )
}

fn picker_point_is_in_srgb(space: ThemeColorSpace, hue: f64, saturation: f64, value: f64) -> bool {
    picker_color(space, hue, saturation, value).is_in_srgb_gamut()
}

/// Normalized line segments tracing the sRGB edge within a Display P3
/// saturation/value square. Coordinates are in the 0..=1 picker domain.
fn srgb_gamut_boundary_segments(hue: f64, cells: u32) -> Vec<[f64; 4]> {
    if cells < 2 {
        return Vec::new();
    }
    let mut gamut = vec![false; (cells * cells) as usize];
    for y in 0..cells {
        for x in 0..cells {
            let saturation = f64::from(x) / f64::from(cells - 1);
            let value = 1.0 - f64::from(y) / f64::from(cells - 1);
            gamut[(y * cells + x) as usize] =
                picker_point_is_in_srgb(ThemeColorSpace::DisplayP3, hue, saturation, value);
        }
    }

    let mut segments = Vec::new();
    let cell = 1.0 / f64::from(cells);
    for y in 0..cells - 1 {
        for x in 0..cells - 1 {
            let here = gamut[(y * cells + x) as usize];
            if here != gamut[(y * cells + x + 1) as usize] {
                let edge_x = f64::from(x + 1) * cell;
                let edge_y = f64::from(y) * cell;
                segments.push([edge_x, edge_y, edge_x, edge_y + cell]);
            }
            if here != gamut[((y + 1) * cells + x) as usize] {
                let edge_x = f64::from(x) * cell;
                let edge_y = f64::from(y + 1) * cell;
                segments.push([edge_x, edge_y, edge_x + cell, edge_y]);
            }
        }
    }
    segments
}

fn set_cairo_theme_color(cr: &cairo::Context, color: ThemeColor, alpha_scale: f64) {
    let color = color.mapped_for_srgb_preview();
    cr.set_source_rgba(
        srgb_encode(color.r).clamp(0.0, 1.0),
        srgb_encode(color.g).clamp(0.0, 1.0),
        srgb_encode(color.b).clamp(0.0, 1.0),
        f64::from(color.a).clamp(0.0, 1.0) * alpha_scale,
    );
}

fn draw_theme_picker(cr: &cairo::Context, width: i32, height: i32, draft: &ThemeEditorDraft) {
    let square_size = f64::from(width.min(height)) - 16.0;
    let center_x = f64::from(width) / 2.0;
    let center_y = f64::from(height) / 2.0;
    let square_x = center_x - square_size / 2.0;
    let square_y = center_y - square_size / 2.0;
    let cells: u32 = 36;
    let cell = square_size / f64::from(cells);
    for y in 0..cells {
        for x in 0..cells {
            let saturation = f64::from(x) / f64::from(cells - 1);
            let value = 1.0 - f64::from(y) / f64::from(cells - 1);
            let color = picker_color(draft.space(), draft.hue, saturation, value);
            set_cairo_theme_color(cr, color, 1.0);
            cr.rectangle(
                square_x + f64::from(x) * cell,
                square_y + f64::from(y) * cell,
                cell + 0.6,
                cell + 0.6,
            );
            let _ = cr.fill();
        }
    }

    if draft.space() == ThemeColorSpace::DisplayP3 {
        let segments = srgb_gamut_boundary_segments(draft.hue, cells);
        let append_boundary = |cr: &cairo::Context| {
            for [x1, y1, x2, y2] in &segments {
                cr.move_to(square_x + x1 * square_size, square_y + y1 * square_size);
                cr.line_to(square_x + x2 * square_size, square_y + y2 * square_size);
            }
        };
        cr.set_dash(&[], 0.0);
        cr.set_line_width(4.0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.72);
        append_boundary(cr);
        let _ = cr.stroke();
        cr.set_dash(&[4.0, 3.0], 0.0);
        cr.set_line_width(1.6);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.96);
        append_boundary(cr);
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
    }

    let marker_x = square_x + draft.saturation * square_size;
    let marker_y = square_y + (1.0 - draft.value) * square_size;
    cr.set_line_width(3.0);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.arc(marker_x, marker_y, 7.0, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke_preserve();
    cr.set_line_width(1.5);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.stroke();
}

fn draw_hue_ring(cr: &cairo::Context, width: i32, height: i32, hue: f64) {
    let center_x = f64::from(width) / 2.0;
    let center_y = f64::from(height) / 2.0;
    let radius = f64::from(width.min(height)) / 2.0 - 18.0;
    let segments = 180;
    cr.set_line_width(24.0);
    for segment in 0..segments {
        let start = f64::from(segment) * std::f64::consts::TAU / f64::from(segments);
        let end = f64::from(segment + 1) * std::f64::consts::TAU / f64::from(segments) + 0.002;
        let segment_hue = f64::from(segment) * 360.0 / f64::from(segments);
        let [r, g, b] = hsv_to_rgb(segment_hue, 1.0, 1.0);
        cr.set_source_rgb(r, g, b);
        cr.arc(center_x, center_y, radius, start, end);
        let _ = cr.stroke();
    }

    let angle = wrap_hue(hue).to_radians();
    let marker_x = center_x + radius * angle.cos();
    let marker_y = center_y + radius * angle.sin();
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(5.0);
    cr.arc(marker_x, marker_y, 8.0, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.0);
    cr.arc(marker_x, marker_y, 8.0, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
}

fn paint_stop_values(stop: &GradientStop) -> [f64; 5] {
    let color = stop.color.mapped_for_srgb_preview();
    [
        f64::from(stop.position.clamp(0.0, 1.0)),
        srgb_encode(color.r).clamp(0.0, 1.0),
        srgb_encode(color.g).clamp(0.0, 1.0),
        srgb_encode(color.b).clamp(0.0, 1.0),
        f64::from(color.a).clamp(0.0, 1.0),
    ]
}

fn set_cairo_paint(cr: &cairo::Context, paint: &ThemePaint, width: f64, height: f64) {
    match paint {
        ThemePaint::Solid { color } => set_cairo_theme_color(cr, *color, 1.0),
        ThemePaint::LinearGradient { angle, stops, .. } => {
            let angle = f64::from(*angle).to_radians();
            let center = (width * 0.5, height * 0.5);
            let extent = width.hypot(height) * 0.5;
            let offset = (angle.cos() * extent, angle.sin() * extent);
            let gradient = cairo::LinearGradient::new(
                center.0 - offset.0,
                center.1 - offset.1,
                center.0 + offset.0,
                center.1 + offset.1,
            );
            for stop in stops {
                let [position, r, g, b, a] = paint_stop_values(stop);
                gradient.add_color_stop_rgba(position, r, g, b, a);
            }
            let _ = cr.set_source(&gradient);
        }
        ThemePaint::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => {
            let center = (f64::from(center.0) * width, f64::from(center.1) * height);
            let gradient = cairo::RadialGradient::new(
                center.0,
                center.1,
                0.0,
                center.0,
                center.1,
                f64::from(*radius) * width.max(height),
            );
            for stop in stops {
                let [position, r, g, b, a] = paint_stop_values(stop);
                gradient.add_color_stop_rgba(position, r, g, b, a);
            }
            let _ = cr.set_source(&gradient);
        }
    }
}

fn draw_theme_preview(cr: &cairo::Context, width: i32, height: i32, draft: &ThemeEditorDraft) {
    let width = f64::from(width);
    let height = f64::from(height);
    cr.set_source_rgb(0.035, 0.045, 0.065);
    let _ = cr.paint();

    let paint = draft.preview_paint();
    set_cairo_paint(cr, &paint, width, height);
    cr.rectangle(0.0, 0.0, width, 42.0);
    let _ = cr.fill();
    cr.rectangle(0.0, 42.0, 72.0, height - 42.0);
    let _ = cr.fill();

    cr.set_source_rgba(0.08, 0.10, 0.15, 0.94);
    cr.rectangle(98.0, 70.0, width - 126.0, height - 104.0);
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
    cr.rectangle(98.0, 70.0, width - 126.0, 1.0);
    let _ = cr.fill();

    set_cairo_paint(cr, &paint, width - 150.0, 36.0);
    cr.rectangle(114.0, 88.0, width - 158.0, 36.0);
    let _ = cr.fill();

    cr.set_source_rgba(0.96, 0.98, 1.0, 0.96);
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(14.0);
    cr.move_to(126.0, 111.0);
    let _ = cr.show_text("FocalDesk Window");
    cr.set_font_size(12.0);
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.move_to(122.0, 154.0);
    let _ = cr.show_text("Panel, window, menu, accent and text");

    cr.set_source_rgba(0.12, 0.15, 0.21, 0.94);
    cr.rectangle(120.0, 172.0, width * 0.42, 94.0);
    let _ = cr.fill();
    for row in 0..3 {
        if row == 1 {
            set_cairo_paint(cr, &paint, width * 0.38, 22.0);
            cr.rectangle(126.0, 179.0 + row as f64 * 26.0, width * 0.38, 22.0);
            let _ = cr.fill();
        }
        cr.set_source_rgba(0.92, 0.95, 1.0, 0.9);
        cr.move_to(136.0, 195.0 + row as f64 * 26.0);
        let _ = cr.show_text(["New window", "Selected item", "Preferences"][row]);
    }

    set_cairo_theme_color(cr, draft.preview_current_color(), 0.32);
    cr.rectangle(width - 176.0, 166.0, 122.0, 108.0);
    let _ = cr.fill();
    set_cairo_theme_color(cr, draft.preview_current_color(), 1.0);
    cr.arc(width - 115.0, 220.0, 23.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}

fn semantic_surface(theme: &SemanticTheme, index: u32) -> &SurfaceStyle {
    match index {
        1 => &theme.surfaces.dock,
        2 => &theme.surfaces.button,
        3 => &theme.surfaces.active_button,
        4 => &theme.surfaces.popup,
        5 => &theme.surfaces.window_frame,
        _ => &theme.surfaces.bar,
    }
}

fn semantic_surface_mut(theme: &mut SemanticTheme, index: u32) -> &mut SurfaceStyle {
    match index {
        1 => &mut theme.surfaces.dock,
        2 => &mut theme.surfaces.button,
        3 => &mut theme.surfaces.active_button,
        4 => &mut theme.surfaces.popup,
        5 => &mut theme.surfaces.window_frame,
        _ => &mut theme.surfaces.bar,
    }
}

fn interaction_state_from_index(index: u32) -> InteractionState {
    match index {
        1 => InteractionState::Hover,
        2 => InteractionState::Pressed,
        3 => InteractionState::Selected,
        4 => InteractionState::Focused,
        5 => InteractionState::Urgent,
        6 => InteractionState::Disabled,
        _ => InteractionState::Normal,
    }
}

fn draw_semantic_state_preview(
    cr: &cairo::Context,
    width: i32,
    height: i32,
    draft: &ThemeEditorDraft,
    surface_index: u32,
) {
    cr.set_source_rgb(0.035, 0.045, 0.065);
    let _ = cr.paint();
    let style = semantic_surface(&draft.semantic, surface_index);
    let states = [
        ("Normal", InteractionState::Normal),
        ("Hover", InteractionState::Hover),
        ("Focused", InteractionState::Focused),
        ("Urgent", InteractionState::Urgent),
        ("Disabled", InteractionState::Disabled),
    ];
    let gap = 8.0;
    let card_width = (f64::from(width) - gap * 6.0) / 5.0;
    let text = draft.semantic.typography.primary.mapped_for_srgb_preview();
    for (index, (label, state)) in states.into_iter().enumerate() {
        let x = gap + index as f64 * (card_width + gap);
        let color = style.resolve(state).mapped_for_srgb_preview();
        set_cairo_theme_color(cr, color, 1.0);
        cr.rectangle(x, 12.0, card_width, f64::from(height) - 24.0);
        let _ = cr.fill();
        set_cairo_theme_color(cr, text, 1.0);
        cr.set_font_size(12.0);
        cr.move_to(x + 10.0, 38.0);
        let _ = cr.show_text(label);
        let contrast = semantic_contrast_ratio(color, text);
        cr.set_font_size(10.0);
        cr.move_to(x + 10.0, 58.0);
        let _ = cr.show_text(&format!(
            "{contrast:.1}:1{}",
            if contrast < 4.5 { " !" } else { "" }
        ));
    }
}

fn semantic_contrast_ratio(left: ThemeColor, right: ThemeColor) -> f32 {
    let luminance = |color: ThemeColor| {
        let color = color.converted_to(ThemeColorSpace::Srgb);
        0.2126 * color.r.max(0.0) + 0.7152 * color.g.max(0.0) + 0.0722 * color.b.max(0.0)
    };
    let (left, right) = (luminance(left), luminance(right));
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

fn nearest_gradient_stop(stops: &[ThemeEditorStop], position: f32) -> Option<usize> {
    stops
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            (left.position - position)
                .abs()
                .total_cmp(&(right.position - position).abs())
        })
        .map(|(index, _)| index)
}

fn draw_gradient_stop_rail(cr: &cairo::Context, width: i32, height: i32, draft: &ThemeEditorDraft) {
    let width = f64::from(width);
    let height = f64::from(height);
    cr.set_source_rgb(0.08, 0.09, 0.12);
    let _ = cr.paint();
    let paint = draft.preview_paint();
    let stops = match &paint {
        ThemePaint::LinearGradient { stops, .. } | ThemePaint::RadialGradient { stops, .. } => {
            stops.clone()
        }
        ThemePaint::Solid { color } => vec![
            GradientStop {
                position: 0.0,
                color: *color,
            },
            GradientStop {
                position: 1.0,
                color: *color,
            },
        ],
    };
    let rail_paint = ThemePaint::LinearGradient {
        angle: 0.0,
        interpolation: GradientInterpolation {
            space: draft.interpolation_space,
            premultiplied_alpha: true,
        },
        stops,
    };
    set_cairo_paint(cr, &rail_paint, width, height);
    cr.rectangle(8.0, 8.0, width - 16.0, height - 26.0);
    let _ = cr.fill();

    for (index, stop) in draft.stops.iter().enumerate() {
        let x = 8.0 + f64::from(stop.position.clamp(0.0, 1.0)) * (width - 16.0);
        let y = height - 10.0;
        let source_color = if index == draft.selected_stop {
            draft.current_color()
        } else {
            stop.color.color()
        };
        set_cairo_theme_color(cr, draft.preview_color(source_color), 1.0);
        cr.arc(x, y, 7.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(
            if index == draft.selected_stop {
                1.0
            } else {
                0.2
            },
            if index == draft.selected_stop {
                1.0
            } else {
                0.2
            },
            if index == draft.selected_stop {
                1.0
            } else {
                0.2
            },
        );
        cr.set_line_width(if index == draft.selected_stop {
            3.0
        } else {
            1.5
        });
        let _ = cr.stroke();
    }
}

fn update_gradient_stop_accessibility(rail: &gtk::DrawingArea, draft: &ThemeEditorDraft) {
    if draft.mode == 0 || draft.stops.is_empty() {
        return;
    }
    let position = f64::from(draft.stops[draft.selected_stop].position) * 100.0;
    let value = format!(
        "Stop {} of {}, position {:.0} percent",
        draft.selected_stop + 1,
        draft.stops.len(),
        position
    );
    rail.update_property(&[
        gtk::accessible::Property::ValueNow(position),
        gtk::accessible::Property::ValueText(&value),
    ]);
}

fn update_theme_editor_status(label: &gtk::Label, draft: &ThemeEditorDraft) {
    let space = match draft.space() {
        ThemeColorSpace::Srgb => "sRGB",
        ThemeColorSpace::DisplayP3 => "Display P3",
        ThemeColorSpace::Rec2020 => "Rec.2020",
    };
    let gamut = if draft.space() == ThemeColorSpace::DisplayP3 {
        if draft.current_color().is_in_srgb_gamut() {
            "  ·  Inside sRGB gamut"
        } else {
            "  ●  Outside sRGB gamut"
        }
    } else {
        ""
    };
    let paint = match draft.mode {
        1 => format!(
            "Linear · Stop {}/{}  ·  ",
            draft.selected_stop + 1,
            draft.stops.len()
        ),
        2 => format!(
            "Radial · Stop {}/{}  ·  ",
            draft.selected_stop + 1,
            draft.stops.len()
        ),
        _ => "Solid  ·  ".to_string(),
    };
    let dynamic_range = match draft.dynamic_range {
        ThemeDynamicRange::Sdr => "SDR".to_string(),
        ThemeDynamicRange::Hdr => format!("HDR  ·  {:.0} nits", draft.hdr_luminance_nits),
    };
    label.set_text(&format!(
        "{paint}{space}  ·  {dynamic_range}  ·  H {:.0}°  S {:.0}%  V {:.0}%{gamut}",
        draft.hue,
        draft.saturation * 100.0,
        draft.value * 100.0
    ));
    label.remove_css_class("warning");
    if draft.space() == ThemeColorSpace::DisplayP3 && !draft.current_color().is_in_srgb_gamut() {
        label.add_css_class("warning");
    }
}

fn update_theme_editor_accessibility(
    picker: &gtk::DrawingArea,
    hue_slider: &gtk::DrawingArea,
    draft: &ThemeEditorDraft,
) {
    let outside =
        draft.space() == ThemeColorSpace::DisplayP3 && !draft.current_color().is_in_srgb_gamut();
    let picker_value = format!(
        "{} saturation {:.0} percent, value {:.0} percent{}",
        match draft.space() {
            ThemeColorSpace::Srgb => "sRGB",
            ThemeColorSpace::DisplayP3 => "Display P3",
            ThemeColorSpace::Rec2020 => "Rec.2020",
        },
        draft.saturation * 100.0,
        draft.value * 100.0,
        if outside { ", outside sRGB gamut" } else { "" }
    );
    picker.update_property(&[
        gtk::accessible::Property::ValueNow(draft.value * 100.0),
        gtk::accessible::Property::ValueText(&picker_value),
    ]);
    let hue_value = format!("{:.0} degrees", draft.hue);
    hue_slider.update_property(&[
        gtk::accessible::Property::ValueNow(draft.hue),
        gtk::accessible::Property::ValueText(&hue_value),
    ]);
}

fn theme_editor_path_label(path: Option<&std::path::Path>, dirty: bool) -> String {
    let location = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Not saved yet".to_string());
    if dirty {
        format!("{location} · Unsaved changes")
    } else {
        location
    }
}

fn theme_editor_toml_path(mut path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension("toml");
    }
    path
}

fn theme_package_path(mut path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension("fdtheme");
    }
    path
}

fn installed_themes_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("themes")
}

fn extract_wallpaper_accent(path: &std::path::Path) -> Result<ThemeColor, String> {
    use image::GenericImageView;
    let image = image::open(path)
        .map_err(|error| error.to_string())?
        .thumbnail(96, 96);
    let mut sum = [0.0f64; 3];
    let mut weight = 0.0;
    for (_, _, pixel) in image.pixels() {
        let rgb = [
            f64::from(pixel[0]) / 255.0,
            f64::from(pixel[1]) / 255.0,
            f64::from(pixel[2]) / 255.0,
        ];
        let maximum = rgb.into_iter().fold(0.0f64, f64::max);
        let minimum = rgb.into_iter().fold(1.0f64, f64::min);
        let saturation = maximum - minimum;
        let luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let pixel_weight = saturation * (1.0 - (luminance - 0.55).abs()).max(0.1);
        if pixel_weight > 0.02 {
            for index in 0..3 {
                sum[index] += rgb[index] * pixel_weight;
            }
            weight += pixel_weight;
        }
    }
    if weight <= f64::EPSILON {
        return Err("wallpaper has no extractable chromatic accent".to_string());
    }
    Ok(ThemeColor::srgb(
        srgb_decode(sum[0] / weight) as f32,
        srgb_decode(sum[1] / weight) as f32,
        srgb_decode(sum[2] / weight) as f32,
        1.0,
    ))
}

fn persist_theme_editor_document(
    draft: &Rc<RefCell<ThemeEditorDraft>>,
    saved_document: &Rc<RefCell<ThemeDocument>>,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    path: PathBuf,
) -> Result<(), String> {
    let path = theme_editor_toml_path(path);
    let document = draft.borrow().document();
    document.save(&path).map_err(|error| error.to_string())?;
    *saved_document.borrow_mut() = document;
    *current_path.borrow_mut() = Some(path);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThemeEditorRuntimeStatus {
    preview_active: bool,
    applied_revision: u64,
    gradient_rendering: bool,
    semantic_rendering: bool,
    wallpaper_processing: bool,
    layout_metrics: bool,
    typography_metrics: bool,
    contrast_issue_count: usize,
}

#[derive(Debug)]
enum ThemeEditorIpcJob {
    Probe,
    Preview(ThemeDocument),
    Apply(ThemeDocument),
    Revert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeEditorIpcAction {
    Probe,
    Preview,
    Apply,
    Revert,
}

#[derive(Debug)]
struct ThemeEditorIpcResult {
    action: ThemeEditorIpcAction,
    gradient: bool,
    result: Result<ThemeEditorRuntimeStatus, String>,
}

fn spawn_theme_editor_ipc_worker() -> (
    mpsc::Sender<ThemeEditorIpcJob>,
    mpsc::Receiver<ThemeEditorIpcResult>,
) {
    let (job_tx, job_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(job) = job_rx.recv() {
            let (action, gradient, command) = match job {
                ThemeEditorIpcJob::Probe => (ThemeEditorIpcAction::Probe, false, None),
                ThemeEditorIpcJob::Preview(document) => {
                    let gradient = !matches!(document.intent.paint, ThemePaint::Solid { .. });
                    (
                        ThemeEditorIpcAction::Preview,
                        gradient,
                        Some(ThemeEditorCommand::Preview { document }),
                    )
                }
                ThemeEditorIpcJob::Apply(document) => {
                    let gradient = !matches!(document.intent.paint, ThemePaint::Solid { .. });
                    (
                        ThemeEditorIpcAction::Apply,
                        gradient,
                        Some(ThemeEditorCommand::Apply { document }),
                    )
                }
                ThemeEditorIpcJob::Revert => (
                    ThemeEditorIpcAction::Revert,
                    false,
                    Some(ThemeEditorCommand::Revert),
                ),
            };
            let result = send_theme_editor_command(command);
            if result_tx
                .send(ThemeEditorIpcResult {
                    action,
                    gradient,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (job_tx, result_rx)
}

fn send_theme_editor_command(
    command: Option<ThemeEditorCommand>,
) -> Result<ThemeEditorRuntimeStatus, String> {
    let request = match command {
        Some(command) => IpcRequest::ThemeEditor {
            protocol_version: THEME_EDITOR_PROTOCOL_VERSION,
            command,
        },
        None => IpcRequest::GetThemeEditorStatus {
            protocol_version: THEME_EDITOR_PROTOCOL_VERSION,
        },
    };
    match send_desktop_request(&request)? {
        IpcResponse::ThemeEditorStatus {
            protocol_version,
            preview_active,
            applied_revision,
            gradient_rendering,
            semantic_rendering,
            wallpaper_processing,
            layout_metrics,
            typography_metrics,
            contrast_issue_count,
        } if protocol_version == THEME_EDITOR_PROTOCOL_VERSION => Ok(ThemeEditorRuntimeStatus {
            preview_active,
            applied_revision,
            gradient_rendering,
            semantic_rendering,
            wallpaper_processing,
            layout_metrics,
            typography_metrics,
            contrast_issue_count,
        }),
        IpcResponse::ThemeEditorStatus {
            protocol_version, ..
        } => Err(format!(
            "unsupported compositor theme protocol {protocol_version}; editor requires {THEME_EDITOR_PROTOCOL_VERSION}"
        )),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected compositor response: {other:?}")),
    }
}

fn theme_editor_runtime_label(status: ThemeEditorRuntimeStatus, gradient: bool) -> String {
    if !status.semantic_rendering {
        "Connected · compositor lacks semantic theme rendering".to_string()
    } else if !status.wallpaper_processing || !status.layout_metrics || !status.typography_metrics {
        "Connected · compositor has partial semantic renderer support".to_string()
    } else if gradient && !status.gradient_rendering {
        "Connected · gradients preview at their midpoint".to_string()
    } else if status.preview_active {
        format!(
            "Connected · preview active · {} contrast issue{}",
            status.contrast_issue_count,
            if status.contrast_issue_count == 1 {
                ""
            } else {
                "s"
            }
        )
    } else {
        format!("Connected · applied revision {}", status.applied_revision)
    }
}

fn theme_editor_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Theme Editor");
    page.set_description(
        "Phase 10: author semantic surfaces, interaction states, edges, typography, layout, and color behavior.",
    );
    let surfaces_page = adw::PreferencesPage::new();
    surfaces_page.set_title("Surfaces");
    surfaces_page
        .set_description("Tune surface colors, interaction states, edges, and typography.");
    let layout_page = adw::PreferencesPage::new();
    layout_page.set_title("Layout & Color");
    layout_page.set_description("Adjust shell geometry and output color behavior.");
    let wallpaper_page = adw::PreferencesPage::new();
    wallpaper_page.set_title("Wallpaper");
    wallpaper_page.set_description("Choose an image and control its relationship to the theme.");
    let paint_page = adw::PreferencesPage::new();
    paint_page.set_title("Paint");
    paint_page
        .set_description("Author solid colors and gradients with the live picker and preview.");

    let editor_root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let section_stack = gtk::Stack::new();
    section_stack.set_hexpand(true);
    section_stack.set_vexpand(true);
    section_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    section_stack.add_titled(&page, Some("general"), "General");
    section_stack.add_titled(&surfaces_page, Some("surfaces"), "Surfaces");
    section_stack.add_titled(&layout_page, Some("layout"), "Layout & Color");
    section_stack.add_titled(&wallpaper_page, Some("wallpaper"), "Wallpaper");
    section_stack.add_titled(&paint_page, Some("paint"), "Paint");

    let section_switcher = gtk::StackSwitcher::new();
    section_switcher.set_stack(Some(&section_stack));
    section_switcher.set_halign(gtk::Align::Center);
    section_switcher.set_margin_top(8);
    section_switcher.set_margin_bottom(8);
    section_switcher.set_margin_start(12);
    section_switcher.set_margin_end(12);
    section_switcher.add_css_class("linked");
    editor_root.append(&section_switcher);
    editor_root.append(&section_stack);

    let draft = Rc::new(RefCell::new(ThemeEditorDraft::new(205.0, 0.88, 1.0, 1.0)));
    let saved_document = Rc::new(RefCell::new(draft.borrow().document()));
    let current_path = Rc::new(RefCell::new(None::<PathBuf>));
    let installed_slug = Rc::new(RefCell::new(None::<String>));
    let last_preview_document = Rc::new(RefCell::new(None::<ThemeDocument>));
    let compositor_connected = Rc::new(Cell::new(false));
    let editor_page_active = Rc::new(Cell::new(false));
    let preview_in_flight = Rc::new(Cell::new(false));
    let (theme_ipc_tx, theme_ipc_rx) = spawn_theme_editor_ipc_worker();
    let theme_ipc_rx = Rc::new(RefCell::new(theme_ipc_rx));

    let document_group = adw::PreferencesGroup::new();
    document_group.set_title("Theme File");
    let name_row = adw::ActionRow::new();
    name_row.set_title("Name");
    let theme_name = gtk::Entry::new();
    theme_name.set_text(&draft.borrow().theme_name);
    theme_name.set_width_chars(28);
    name_row.add_suffix(&theme_name);
    document_group.add(&name_row);

    let file_row = adw::ActionRow::new();
    file_row.set_title("TOML document");
    file_row.set_subtitle("Not saved yet");
    let file_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let open_theme = gtk::Button::with_label("Open…");
    let save_theme = gtk::Button::with_label("Save");
    save_theme.set_sensitive(false);
    let save_theme_as = gtk::Button::with_label("Save As…");
    let revert_theme = gtk::Button::with_label("Revert");
    revert_theme.set_sensitive(false);
    file_actions.append(&open_theme);
    file_actions.append(&save_theme);
    file_actions.append(&save_theme_as);
    file_actions.append(&revert_theme);
    file_row.add_suffix(&file_actions);
    document_group.add(&file_row);

    let file_message = gtk::Label::new(Some("Ready"));
    file_message.set_xalign(0.0);
    file_message.set_wrap(true);
    file_message.add_css_class("dim-label");
    document_group.add(&file_message);
    page.add(&document_group);

    let compositor_group = adw::PreferencesGroup::new();
    compositor_group.set_title("Live Compositor");
    compositor_group.set_description(Some(
        "Preview is temporary. Apply keeps the runtime theme; leaving the editor restores the last applied theme.",
    ));
    let live_preview_row = adw::ActionRow::new();
    live_preview_row.set_title("Live preview");
    live_preview_row.set_subtitle("Debounced to avoid flooding compositor IPC");
    let live_preview = gtk::Switch::new();
    live_preview.set_active(true);
    live_preview_row.add_suffix(&live_preview);
    live_preview_row.set_activatable_widget(Some(&live_preview));
    compositor_group.add(&live_preview_row);

    let runtime_row = adw::ActionRow::new();
    runtime_row.set_title("Runtime theme");
    runtime_row.set_subtitle("Checking compositor…");
    let runtime_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let apply_runtime = gtk::Button::with_label("Apply");
    let revert_runtime = gtk::Button::with_label("Revert Preview");
    apply_runtime.set_sensitive(false);
    revert_runtime.set_sensitive(false);
    runtime_actions.append(&apply_runtime);
    runtime_actions.append(&revert_runtime);
    runtime_row.add_suffix(&runtime_actions);
    compositor_group.add(&runtime_row);
    page.add(&compositor_group);

    let wallpaper_group = adw::PreferencesGroup::new();
    wallpaper_group.set_title("Wallpaper");
    let wallpaper_picture = gtk::Picture::new();
    wallpaper_picture.set_content_fit(gtk::ContentFit::Cover);
    wallpaper_picture.set_size_request(-1, 180);
    wallpaper_picture.add_css_class("card");
    wallpaper_group.add(&wallpaper_picture);
    let wallpaper_row = adw::ActionRow::new();
    wallpaper_row.set_title("Image asset");
    wallpaper_row.set_subtitle("No wallpaper selected");
    let wallpaper_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let choose_wallpaper = gtk::Button::with_label("Choose…");
    let clear_wallpaper = gtk::Button::with_label("Clear");
    clear_wallpaper.set_sensitive(false);
    wallpaper_actions.append(&choose_wallpaper);
    wallpaper_actions.append(&clear_wallpaper);
    wallpaper_row.add_suffix(&wallpaper_actions);
    wallpaper_group.add(&wallpaper_row);
    let wallpaper_fit_row = adw::ActionRow::new();
    wallpaper_fit_row.set_title("Fit mode");
    let wallpaper_fit = dropdown_from_strings(&["Fill", "Fit", "Stretch", "Center", "Tile"], 0);
    wallpaper_fit_row.add_suffix(&wallpaper_fit);
    wallpaper_group.add(&wallpaper_fit_row);
    let wallpaper_dim_row = adw::ActionRow::new();
    wallpaper_dim_row.set_title("Dimming");
    let wallpaper_dim = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    wallpaper_dim.set_value(0.0);
    wallpaper_dim.set_digits(2);
    wallpaper_dim.set_draw_value(true);
    wallpaper_dim.set_size_request(280, -1);
    wallpaper_dim_row.add_suffix(&wallpaper_dim);
    wallpaper_group.add(&wallpaper_dim_row);
    let wallpaper_tint_row = adw::ActionRow::new();
    wallpaper_tint_row.set_title("Tint");
    let wallpaper_tint = gtk::Switch::new();
    wallpaper_tint_row.add_suffix(&wallpaper_tint);
    wallpaper_tint_row.set_activatable_widget(Some(&wallpaper_tint));
    wallpaper_group.add(&wallpaper_tint_row);
    let tint_values = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    tint_values.set_sensitive(false);
    let mut wallpaper_tint_spins = Vec::new();
    for (name, value) in [("R", 0.1), ("G", 0.2), ("B", 0.4), ("A", 0.3)] {
        tint_values.append(&gtk::Label::new(Some(name)));
        let spin = gtk::SpinButton::with_range(0.0, 1.0, 0.01);
        spin.set_digits(2);
        spin.set_value(value);
        tint_values.append(&spin);
        wallpaper_tint_spins.push(spin);
    }
    wallpaper_group.add(&tint_values);
    wallpaper_page.add(&wallpaper_group);

    let package_group = adw::PreferencesGroup::new();
    package_group.set_title("Theme Package");
    package_group.set_description(Some(
        "Portable .fdtheme packages embed the validated TOML document and wallpaper asset.",
    ));
    let package_row = adw::ActionRow::new();
    package_row.set_title("Installable package");
    package_row.set_subtitle("Not installed");
    let package_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let export_package = gtk::Button::with_label("Export…");
    let import_package = gtk::Button::with_label("Import…");
    let uninstall_package = gtk::Button::with_label("Uninstall");
    uninstall_package.set_sensitive(false);
    package_actions.append(&export_package);
    package_actions.append(&import_package);
    package_actions.append(&uninstall_package);
    package_row.add_suffix(&package_actions);
    package_group.add(&package_row);
    page.add(&package_group);

    let semantic_group = adw::PreferencesGroup::new();
    semantic_group.set_title("Surfaces and Interaction");
    let semantic_surface_row = adw::ActionRow::new();
    semantic_surface_row.set_title("Surface");
    let semantic_surface_select = dropdown_from_strings(
        &[
            "Bar",
            "Dock",
            "Button",
            "Active Button",
            "Popup",
            "Window Frame",
        ],
        0,
    );
    semantic_surface_row.add_suffix(&semantic_surface_select);
    semantic_group.add(&semantic_surface_row);
    let semantic_state_row = adw::ActionRow::new();
    semantic_state_row.set_title("Interaction state");
    let semantic_state_select = dropdown_from_strings(
        &[
            "Normal", "Hover", "Pressed", "Selected", "Focused", "Urgent", "Disabled",
        ],
        0,
    );
    semantic_state_row.add_suffix(&semantic_state_select);
    semantic_group.add(&semantic_state_row);
    let semantic_override_row = adw::ActionRow::new();
    semantic_override_row.set_title("Override inherited color");
    let semantic_override = gtk::Switch::new();
    semantic_override.set_sensitive(false);
    semantic_override_row.add_suffix(&semantic_override);
    semantic_group.add(&semantic_override_row);
    let semantic_color_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mut semantic_color_spins = Vec::new();
    for name in ["R", "G", "B", "A"] {
        semantic_color_box.append(&gtk::Label::new(Some(name)));
        let spin = gtk::SpinButton::with_range(0.0, 1.0, 0.01);
        spin.set_digits(2);
        semantic_color_box.append(&spin);
        semantic_color_spins.push(spin);
    }
    semantic_group.add(&semantic_color_box);
    let semantic_preview = gtk::DrawingArea::new();
    semantic_preview.set_content_height(105);
    semantic_preview.set_hexpand(true);
    {
        let draft = draft.clone();
        let semantic_surface_select = semantic_surface_select.clone();
        semantic_preview.set_draw_func(move |_, cr, width, height| {
            draw_semantic_state_preview(
                cr,
                width,
                height,
                &draft.borrow(),
                semantic_surface_select.selected(),
            );
        });
    }
    semantic_group.add(&semantic_preview);
    surfaces_page.add(&semantic_group);

    let edges_group = adw::PreferencesGroup::new();
    edges_group.set_title("Edges");
    let border_color_row = adw::ActionRow::new();
    border_color_row.set_title("Border color");
    let semantic_border_color = gtk::ColorDialogButton::new(None::<gtk::ColorDialog>);
    border_color_row.add_suffix(&semantic_border_color);
    edges_group.add(&border_color_row);
    let highlight_color_row = adw::ActionRow::new();
    highlight_color_row.set_title("Inner highlight");
    let semantic_inner_highlight = gtk::ColorDialogButton::new(None::<gtk::ColorDialog>);
    highlight_color_row.add_suffix(&semantic_inner_highlight);
    edges_group.add(&highlight_color_row);
    let semantic_border_width = add_scale_row(&edges_group, "Border width", 0.0, 16.0, 0.5, 1.0);
    let semantic_radius = add_scale_row(&edges_group, "Radius", 0.0, 64.0, 1.0, 10.0);
    let semantic_shadow = add_scale_row(&edges_group, "Shadow", 0.0, 1.0, 0.01, 0.4);
    let semantic_glow = add_scale_row(&edges_group, "Glow", 0.0, 1.0, 0.01, 0.15);
    surfaces_page.add(&edges_group);

    let typography_group = adw::PreferencesGroup::new();
    typography_group.set_title("Typography");
    let primary_text_row = adw::ActionRow::new();
    primary_text_row.set_title("Primary text");
    let semantic_primary_text = gtk::ColorDialogButton::new(None::<gtk::ColorDialog>);
    semantic_primary_text.set_rgba(&gtk::gdk::RGBA::new(0.96, 0.98, 1.0, 1.0));
    primary_text_row.add_suffix(&semantic_primary_text);
    typography_group.add(&primary_text_row);
    let secondary_text_row = adw::ActionRow::new();
    secondary_text_row.set_title("Secondary text");
    let semantic_secondary_text = gtk::ColorDialogButton::new(None::<gtk::ColorDialog>);
    semantic_secondary_text.set_rgba(&gtk::gdk::RGBA::new(0.72, 0.78, 0.86, 1.0));
    secondary_text_row.add_suffix(&semantic_secondary_text);
    typography_group.add(&secondary_text_row);
    let semantic_font_weight =
        add_scale_row(&typography_group, "Font weight", 100.0, 900.0, 100.0, 500.0);
    let semantic_font_size = add_scale_row(&typography_group, "Font size", 8.0, 72.0, 1.0, 14.0);
    let semantic_letter_spacing =
        add_scale_row(&typography_group, "Letter spacing", -2.0, 8.0, 0.1, 0.0);
    surfaces_page.add(&typography_group);

    let layout_group = adw::PreferencesGroup::new();
    layout_group.set_title("Layout");
    let semantic_bar_height = add_scale_row(&layout_group, "Bar height", 24.0, 96.0, 1.0, 36.0);
    let semantic_dock_width = add_scale_row(&layout_group, "Dock width", 40.0, 160.0, 1.0, 64.0);
    let semantic_padding = add_scale_row(&layout_group, "Padding", 0.0, 48.0, 1.0, 12.0);
    let semantic_gap = add_scale_row(&layout_group, "Gaps", 0.0, 48.0, 1.0, 8.0);
    let semantic_icon_size = add_scale_row(&layout_group, "Icon size", 12.0, 96.0, 1.0, 24.0);
    layout_page.add(&layout_group);

    let color_behavior_group = adw::PreferencesGroup::new();
    color_behavior_group.set_title("Color Behavior");
    let semantic_sdr_white =
        add_scale_row(&color_behavior_group, "SDR white", 80.0, 400.0, 1.0, 203.0);
    let semantic_luminance_cap = add_scale_row(
        &color_behavior_group,
        "Luminance cap",
        203.0,
        10_000.0,
        10.0,
        1_000.0,
    );
    let gamut_mapping_row = adw::ActionRow::new();
    gamut_mapping_row.set_title("Gamut mapping");
    let semantic_gamut_mapping = dropdown_from_strings(&["Clip", "Perceptual", "Preserve Hue"], 1);
    gamut_mapping_row.add_suffix(&semantic_gamut_mapping);
    color_behavior_group.add(&gamut_mapping_row);
    layout_page.add(&color_behavior_group);

    let wallpaper_processing_group = adw::PreferencesGroup::new();
    wallpaper_processing_group.set_title("Wallpaper Integration");
    let semantic_wallpaper_blur =
        add_scale_row(&wallpaper_processing_group, "Blur", 0.0, 64.0, 1.0, 0.0);
    let semantic_wallpaper_saturation = add_scale_row(
        &wallpaper_processing_group,
        "Saturation",
        0.0,
        2.0,
        0.01,
        1.0,
    );
    let auto_accent_row = adw::ActionRow::new();
    auto_accent_row.set_title("Automatic accent extraction");
    let semantic_auto_accent = gtk::Switch::new();
    auto_accent_row.add_suffix(&semantic_auto_accent);
    wallpaper_processing_group.add(&auto_accent_row);
    wallpaper_page.add(&wallpaper_processing_group);

    let phase = adw::PreferencesGroup::new();
    phase.set_title("Paint");
    let mode_row = adw::ActionRow::new();
    mode_row.set_title("Color mode");
    let mode = dropdown_from_strings(&["Solid", "Linear Gradient", "Radial Gradient"], 0);
    mode_row.add_suffix(&mode);
    phase.add(&mode_row);
    let phase_row = adw::ActionRow::new();
    phase_row.set_title("Gamut");
    phase_row.set_subtitle("The picker geometry stays the same in either gamut");
    let gamut = dropdown_from_strings(&["sRGB", "Display P3"], 0);
    phase_row.add_suffix(&gamut);
    phase.add(&phase_row);

    let dynamic_range_row = adw::ActionRow::new();
    dynamic_range_row.set_title("Dynamic range");
    dynamic_range_row.set_subtitle("HDR changes luminance intent, not picker geometry");
    let dynamic_range = dropdown_from_strings(&["SDR", "HDR"], 0);
    dynamic_range_row.add_suffix(&dynamic_range);
    phase.add(&dynamic_range_row);

    let hdr_luminance_row = adw::ActionRow::new();
    hdr_luminance_row.set_title("HDR luminance");
    hdr_luminance_row.set_subtitle("SDR-mapped locally; source intent remains HDR");
    let hdr_luminance = gtk::Scale::with_range(gtk::Orientation::Horizontal, 203.0, 1_000.0, 1.0);
    hdr_luminance.set_value(1_000.0);
    hdr_luminance.set_digits(0);
    hdr_luminance.set_draw_value(true);
    hdr_luminance.set_size_request(320, -1);
    hdr_luminance.set_sensitive(false);
    hdr_luminance_row.add_suffix(&hdr_luminance);
    phase.add(&hdr_luminance_row);
    paint_page.add(&phase);

    let gradient_group = adw::PreferencesGroup::new();
    gradient_group.set_title("Gradient");
    gradient_group.set_visible(false);
    let stop_rail = gtk::DrawingArea::new();
    stop_rail.set_content_height(72);
    stop_rail.set_hexpand(true);
    stop_rail.set_focusable(true);
    stop_rail.set_accessible_role(gtk::AccessibleRole::Slider);
    stop_rail.update_property(&[
        gtk::accessible::Property::Label("Gradient stops"),
        gtk::accessible::Property::Description(
            "Select or drag a stop. Left and Right move the selected stop; hold Shift for larger steps.",
        ),
        gtk::accessible::Property::ValueMin(0.0),
        gtk::accessible::Property::ValueMax(100.0),
    ]);
    {
        let draft = draft.clone();
        stop_rail.set_draw_func(move |_, cr, width, height| {
            draw_gradient_stop_rail(cr, width, height, &draft.borrow());
        });
    }
    gradient_group.add(&stop_rail);

    let stop_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let add_stop = gtk::Button::with_label("Add Stop");
    let duplicate_stop = gtk::Button::with_label("Duplicate");
    let remove_stop = gtk::Button::with_label("Remove");
    stop_actions.append(&add_stop);
    stop_actions.append(&duplicate_stop);
    stop_actions.append(&remove_stop);
    gradient_group.add(&stop_actions);

    let position_row = adw::ActionRow::new();
    position_row.set_title("Selected stop position");
    let stop_position = gtk::SpinButton::with_range(0.0, 1.0, 0.01);
    stop_position.set_digits(2);
    stop_position.set_value(0.0);
    position_row.add_suffix(&stop_position);
    gradient_group.add(&position_row);

    let interpolation_row = adw::ActionRow::new();
    interpolation_row.set_title("Interpolation gamut");
    let interpolation = dropdown_from_strings(&["Linear sRGB", "Linear Display P3"], 0);
    interpolation_row.add_suffix(&interpolation);
    gradient_group.add(&interpolation_row);

    let angle_row = adw::ActionRow::new();
    angle_row.set_title("Linear angle");
    let linear_angle = gtk::SpinButton::with_range(0.0, 360.0, 1.0);
    linear_angle.set_digits(0);
    linear_angle.set_value(135.0);
    angle_row.add_suffix(&linear_angle);
    gradient_group.add(&angle_row);

    let radial_center_x_row = adw::ActionRow::new();
    radial_center_x_row.set_title("Radial center X");
    let radial_center_x = gtk::SpinButton::with_range(0.0, 1.0, 0.01);
    radial_center_x.set_digits(2);
    radial_center_x.set_value(0.5);
    radial_center_x_row.add_suffix(&radial_center_x);
    radial_center_x_row.set_visible(false);
    gradient_group.add(&radial_center_x_row);

    let radial_center_y_row = adw::ActionRow::new();
    radial_center_y_row.set_title("Radial center Y");
    let radial_center_y = gtk::SpinButton::with_range(0.0, 1.0, 0.01);
    radial_center_y.set_digits(2);
    radial_center_y.set_value(0.5);
    radial_center_y_row.add_suffix(&radial_center_y);
    radial_center_y_row.set_visible(false);
    gradient_group.add(&radial_center_y_row);

    let radial_radius_row = adw::ActionRow::new();
    radial_radius_row.set_title("Radial radius");
    let radial_radius = gtk::SpinButton::with_range(0.01, 2.0, 0.01);
    radial_radius.set_digits(2);
    radial_radius.set_value(0.75);
    radial_radius_row.add_suffix(&radial_radius);
    radial_radius_row.set_visible(false);
    gradient_group.add(&radial_radius_row);
    paint_page.add(&gradient_group);

    let editor_group = adw::PreferencesGroup::new();
    editor_group.set_title("Picker and Preview");
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    columns.set_homogeneous(true);

    let picker_column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let picker = gtk::DrawingArea::new();
    picker.set_content_width(238);
    picker.set_content_height(238);
    picker.set_halign(gtk::Align::Center);
    picker.set_valign(gtk::Align::Center);
    picker.set_focusable(true);
    picker.set_accessible_role(gtk::AccessibleRole::Slider);
    picker.update_property(&[
        gtk::accessible::Property::Label("Saturation and value"),
        gtk::accessible::Property::Description(
            "Drag to choose saturation and value. Arrow keys adjust the selection; hold Shift for larger steps.",
        ),
        gtk::accessible::Property::ValueMin(0.0),
        gtk::accessible::Property::ValueMax(100.0),
    ]);
    picker.set_tooltip_text(Some(
        "Saturation/value — drag, or use arrow keys (Shift for larger steps)",
    ));
    {
        let draft = draft.clone();
        picker.set_draw_func(move |_, cr, width, height| {
            draw_theme_picker(cr, width, height, &draft.borrow());
        });
    }
    let hue_slider = gtk::DrawingArea::new();
    hue_slider.set_content_width(340);
    hue_slider.set_content_height(340);
    hue_slider.set_halign(gtk::Align::Center);
    hue_slider.set_focusable(true);
    hue_slider.set_accessible_role(gtk::AccessibleRole::Slider);
    hue_slider.update_property(&[
        gtk::accessible::Property::Label("Hue"),
        gtk::accessible::Property::Description(
            "Drag around the ring to choose hue. Arrow keys adjust one degree; hold Shift for ten degrees.",
        ),
        gtk::accessible::Property::ValueMin(0.0),
        gtk::accessible::Property::ValueMax(360.0),
        gtk::accessible::Property::ValueNow(205.0),
    ]);
    hue_slider.set_tooltip_text(Some(
        "Hue ring — drag, or use arrow keys (Shift for 10° steps)",
    ));
    {
        let draft = draft.clone();
        hue_slider.set_draw_func(move |_, cr, width, height| {
            draw_hue_ring(cr, width, height, draft.borrow().hue);
        });
    }
    let picker_overlay = gtk::Overlay::new();
    picker_overlay.set_child(Some(&hue_slider));
    picker_overlay.add_overlay(&picker);
    picker_column.append(&picker_overlay);

    let gamut_status = gtk::Label::new(None);
    gamut_status.set_xalign(0.5);
    update_theme_editor_status(&gamut_status, &draft.borrow());
    update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
    picker_column.append(&gamut_status);

    let values = gtk::Grid::new();
    values.set_column_spacing(8);
    values.set_row_spacing(8);
    let encoded = hsv_to_rgb(205.0, 0.88, 1.0);
    let mut spins = Vec::new();
    for (column, (name, initial)) in [
        ("R", encoded[0]),
        ("G", encoded[1]),
        ("B", encoded[2]),
        ("A", 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        let label = gtk::Label::new(Some(name));
        let spin = gtk::SpinButton::with_range(0.0, 1.0, 0.001);
        spin.set_digits(3);
        spin.set_value(initial);
        values.attach(&label, column as i32, 0, 1, 1);
        values.attach(&spin, column as i32, 1, 1, 1);
        spins.push(spin);
    }
    picker_column.append(&values);
    columns.append(&picker_column);

    let preview_column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let preview_title = gtk::Label::new(Some("Live FocalDesk surfaces"));
    preview_title.set_xalign(0.0);
    preview_title.add_css_class("heading");
    preview_column.append(&preview_title);
    let preview = gtk::DrawingArea::new();
    preview.set_content_width(440);
    preview.set_content_height(340);
    preview.set_hexpand(true);
    preview.set_vexpand(true);
    {
        let draft = draft.clone();
        preview.set_draw_func(move |_, cr, width, height| {
            draw_theme_preview(cr, width, height, &draft.borrow());
        });
    }
    preview_column.append(&preview);
    let preview_range_status = gtk::Label::new(Some("SDR preview"));
    preview_range_status.set_xalign(0.0);
    preview_range_status.add_css_class("dim-label");
    preview_column.append(&preview_range_status);

    columns.append(&preview_column);
    editor_group.add(&columns);
    paint_page.add(&editor_group);

    let syncing = Rc::new(Cell::new(false));
    let sync_numeric = {
        let spins = spins.clone();
        let gamut = gamut.clone();
        let stop_position = stop_position.clone();
        let syncing = syncing.clone();
        Rc::new(move |draft: &ThemeEditorDraft| {
            syncing.set(true);
            let rgb = hsv_to_rgb(draft.hue, draft.saturation, draft.value);
            for (spin, value) in spins[..3].iter().zip(rgb) {
                spin.set_value(value);
            }
            spins[3].set_value(draft.alpha);
            gamut.set_selected(if draft.space() == ThemeColorSpace::Srgb {
                0
            } else {
                1
            });
            if draft.mode != 0 {
                stop_position.set_value(f64::from(draft.stops[draft.selected_stop].position));
            }
            syncing.set(false);
        })
    };

    let sync_document_ui = {
        let theme_name = theme_name.clone();
        let mode = mode.clone();
        let gamut = gamut.clone();
        let dynamic_range = dynamic_range.clone();
        let hdr_luminance = hdr_luminance.clone();
        let interpolation = interpolation.clone();
        let stop_position = stop_position.clone();
        let linear_angle = linear_angle.clone();
        let radial_center_x = radial_center_x.clone();
        let radial_center_y = radial_center_y.clone();
        let radial_radius = radial_radius.clone();
        let gradient_group = gradient_group.clone();
        let angle_row = angle_row.clone();
        let radial_center_x_row = radial_center_x_row.clone();
        let radial_center_y_row = radial_center_y_row.clone();
        let radial_radius_row = radial_radius_row.clone();
        let remove_stop = remove_stop.clone();
        let preview_range_status = preview_range_status.clone();
        let wallpaper_picture = wallpaper_picture.clone();
        let wallpaper_row = wallpaper_row.clone();
        let clear_wallpaper = clear_wallpaper.clone();
        let semantic_preview = semantic_preview.clone();
        let wallpaper_fit = wallpaper_fit.clone();
        let wallpaper_dim = wallpaper_dim.clone();
        let wallpaper_tint = wallpaper_tint.clone();
        let tint_values = tint_values.clone();
        let wallpaper_tint_spins = wallpaper_tint_spins.clone();
        let semantic_color_spins = semantic_color_spins.clone();
        let semantic_surface_select = semantic_surface_select.clone();
        let semantic_state_select = semantic_state_select.clone();
        let semantic_override = semantic_override.clone();
        let semantic_border_width = semantic_border_width.clone();
        let semantic_border_color = semantic_border_color.clone();
        let semantic_inner_highlight = semantic_inner_highlight.clone();
        let semantic_radius = semantic_radius.clone();
        let semantic_shadow = semantic_shadow.clone();
        let semantic_glow = semantic_glow.clone();
        let semantic_font_weight = semantic_font_weight.clone();
        let semantic_primary_text = semantic_primary_text.clone();
        let semantic_secondary_text = semantic_secondary_text.clone();
        let semantic_font_size = semantic_font_size.clone();
        let semantic_letter_spacing = semantic_letter_spacing.clone();
        let semantic_bar_height = semantic_bar_height.clone();
        let semantic_dock_width = semantic_dock_width.clone();
        let semantic_padding = semantic_padding.clone();
        let semantic_gap = semantic_gap.clone();
        let semantic_icon_size = semantic_icon_size.clone();
        let semantic_sdr_white = semantic_sdr_white.clone();
        let semantic_luminance_cap = semantic_luminance_cap.clone();
        let semantic_gamut_mapping = semantic_gamut_mapping.clone();
        let semantic_wallpaper_blur = semantic_wallpaper_blur.clone();
        let semantic_wallpaper_saturation = semantic_wallpaper_saturation.clone();
        let semantic_auto_accent = semantic_auto_accent.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let syncing = syncing.clone();
        let sync_numeric = sync_numeric.clone();
        Rc::new(move |draft: &ThemeEditorDraft| {
            syncing.set(true);
            theme_name.set_text(&draft.theme_name);
            mode.set_selected(draft.mode);
            gamut.set_selected(if draft.space() == ThemeColorSpace::Srgb {
                0
            } else {
                1
            });
            let is_hdr = draft.dynamic_range == ThemeDynamicRange::Hdr;
            dynamic_range.set_selected(u32::from(is_hdr));
            hdr_luminance.set_value(draft.hdr_luminance_nits);
            hdr_luminance.set_sensitive(is_hdr);
            interpolation.set_selected(if draft.interpolation_space == ThemeColorSpace::Srgb {
                0
            } else {
                1
            });
            if draft.mode != 0 {
                stop_position.set_value(f64::from(draft.stops[draft.selected_stop].position));
            }
            linear_angle.set_value(draft.linear_angle);
            radial_center_x.set_value(draft.radial_center.0);
            radial_center_y.set_value(draft.radial_center.1);
            radial_radius.set_value(draft.radial_radius);
            gradient_group.set_visible(draft.mode != 0);
            angle_row.set_visible(draft.mode == 1);
            radial_center_x_row.set_visible(draft.mode == 2);
            radial_center_y_row.set_visible(draft.mode == 2);
            radial_radius_row.set_visible(draft.mode == 2);
            remove_stop.set_sensitive(draft.stops.len() > 2);
            let preview_status = if is_hdr {
                format!(
                    "HDR · {:.0} nits · SDR-mapped preview",
                    draft.hdr_luminance_nits
                )
            } else {
                "SDR preview".to_string()
            };
            preview_range_status.set_text(&preview_status);
            if let Some(path) = draft.wallpaper.path.as_deref() {
                wallpaper_picture.set_filename(Some(path));
                wallpaper_row.set_subtitle(path);
                clear_wallpaper.set_sensitive(true);
            } else {
                wallpaper_picture.set_filename(None::<&std::path::Path>);
                wallpaper_row.set_subtitle("No wallpaper selected");
                clear_wallpaper.set_sensitive(false);
            }
            wallpaper_fit.set_selected(match draft.wallpaper.fit {
                ThemeWallpaperFit::Fill => 0,
                ThemeWallpaperFit::Fit => 1,
                ThemeWallpaperFit::Stretch => 2,
                ThemeWallpaperFit::Center => 3,
                ThemeWallpaperFit::Tile => 4,
            });
            wallpaper_dim.set_value(f64::from(draft.wallpaper.dim));
            let tint_enabled = draft.wallpaper.tint.is_some();
            wallpaper_tint.set_active(tint_enabled);
            tint_values.set_sensitive(tint_enabled);
            if let Some(tint) = draft.wallpaper.tint {
                for (spin, value) in wallpaper_tint_spins.iter().zip(tint.components()) {
                    spin.set_value(f64::from(value));
                }
            }
            for (spin, value) in semantic_color_spins
                .iter()
                .zip(draft.semantic.surfaces.bar.normal.components())
            {
                spin.set_value(f64::from(value));
                spin.set_sensitive(true);
            }
            semantic_surface_select.set_selected(0);
            semantic_state_select.set_selected(0);
            semantic_override.set_active(true);
            semantic_override.set_sensitive(false);
            semantic_border_width.set_value(f64::from(draft.semantic.edges.border_width));
            for (button, color) in [
                (&semantic_border_color, draft.semantic.edges.border_color),
                (
                    &semantic_inner_highlight,
                    draft.semantic.edges.inner_highlight,
                ),
            ] {
                button.set_rgba(&gtk::gdk::RGBA::new(
                    srgb_encode(color.r) as f32,
                    srgb_encode(color.g) as f32,
                    srgb_encode(color.b) as f32,
                    color.a,
                ));
            }
            semantic_radius.set_value(f64::from(draft.semantic.edges.radius));
            semantic_shadow.set_value(f64::from(draft.semantic.edges.shadow));
            semantic_glow.set_value(f64::from(draft.semantic.edges.glow));
            semantic_font_weight.set_value(f64::from(draft.semantic.typography.font_weight));
            for (button, color) in [
                (&semantic_primary_text, draft.semantic.typography.primary),
                (
                    &semantic_secondary_text,
                    draft.semantic.typography.secondary,
                ),
            ] {
                button.set_rgba(&gtk::gdk::RGBA::new(
                    srgb_encode(color.r) as f32,
                    srgb_encode(color.g) as f32,
                    srgb_encode(color.b) as f32,
                    color.a,
                ));
            }
            semantic_font_size.set_value(f64::from(draft.semantic.typography.size));
            semantic_letter_spacing.set_value(f64::from(draft.semantic.typography.letter_spacing));
            semantic_bar_height.set_value(f64::from(draft.semantic.layout.bar_height));
            semantic_dock_width.set_value(f64::from(draft.semantic.layout.dock_width));
            semantic_padding.set_value(f64::from(draft.semantic.layout.padding));
            semantic_gap.set_value(f64::from(draft.semantic.layout.gap));
            semantic_icon_size.set_value(f64::from(draft.semantic.layout.icon_size));
            semantic_sdr_white.set_value(f64::from(draft.semantic.color_behavior.sdr_white_nits));
            semantic_luminance_cap
                .set_value(f64::from(draft.semantic.color_behavior.luminance_cap_nits));
            semantic_gamut_mapping.set_selected(
                match draft.semantic.color_behavior.gamut_mapping {
                    focaldesk_themes::GamutMapping::Clip => 0,
                    focaldesk_themes::GamutMapping::Perceptual => 1,
                    focaldesk_themes::GamutMapping::PreserveHue => 2,
                },
            );
            semantic_wallpaper_blur.set_value(f64::from(draft.semantic.wallpaper.blur));
            semantic_wallpaper_saturation.set_value(f64::from(draft.semantic.wallpaper.saturation));
            semantic_auto_accent.set_active(draft.semantic.wallpaper.automatic_accent);
            sync_numeric(draft);
            syncing.set(false);
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            semantic_preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, draft);
            update_theme_editor_accessibility(&picker, &hue_slider, draft);
            update_gradient_stop_accessibility(&stop_rail, draft);
        })
    };

    {
        let draft = draft.clone();
        let syncing = syncing.clone();
        theme_name.connect_changed(move |entry| {
            if !syncing.get() {
                draft.borrow_mut().theme_name = entry.text().to_string();
            }
        });
    }

    {
        let draft = draft.clone();
        let wallpaper_picture = wallpaper_picture.clone();
        let wallpaper_row = wallpaper_row.clone();
        let clear_wallpaper = clear_wallpaper.clone();
        let semantic_preview = semantic_preview.clone();
        choose_wallpaper.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Choose Theme Wallpaper");
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Wallpaper images"));
            for pattern in ["*.png", "*.jpg", "*.jpeg", "*.webp"] {
                filter.add_pattern(pattern);
            }
            dialog.set_default_filter(Some(&filter));
            dialog.open(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, {
                let draft = draft.clone();
                let wallpaper_picture = wallpaper_picture.clone();
                let wallpaper_row = wallpaper_row.clone();
                let clear_wallpaper = clear_wallpaper.clone();
                let semantic_preview = semantic_preview.clone();
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        wallpaper_row.set_subtitle("Wallpaper must be a local image");
                        return;
                    };
                    draft.borrow_mut().wallpaper.path = Some(path.to_string_lossy().into_owned());
                    if draft.borrow().semantic.wallpaper.automatic_accent {
                        if let Ok(accent) = extract_wallpaper_accent(&path) {
                            draft.borrow_mut().semantic.surfaces.active_button.normal = accent;
                            semantic_preview.queue_draw();
                        }
                    }
                    wallpaper_picture.set_filename(Some(&path));
                    wallpaper_row.set_subtitle(&path.display().to_string());
                    clear_wallpaper.set_sensitive(true);
                }
            });
        });
    }

    {
        let draft = draft.clone();
        let wallpaper_picture = wallpaper_picture.clone();
        let wallpaper_row = wallpaper_row.clone();
        let clear_wallpaper = clear_wallpaper.clone();
        clear_wallpaper.clone().connect_clicked(move |_| {
            draft.borrow_mut().wallpaper.path = None;
            wallpaper_picture.set_filename(None::<&std::path::Path>);
            wallpaper_row.set_subtitle("No wallpaper selected");
            clear_wallpaper.set_sensitive(false);
        });
    }

    {
        let draft = draft.clone();
        let syncing = syncing.clone();
        wallpaper_fit.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            draft.borrow_mut().wallpaper.fit = match dropdown.selected() {
                1 => ThemeWallpaperFit::Fit,
                2 => ThemeWallpaperFit::Stretch,
                3 => ThemeWallpaperFit::Center,
                4 => ThemeWallpaperFit::Tile,
                _ => ThemeWallpaperFit::Fill,
            };
        });
    }

    {
        let draft = draft.clone();
        let syncing = syncing.clone();
        wallpaper_dim.connect_value_changed(move |scale| {
            if !syncing.get() {
                draft.borrow_mut().wallpaper.dim = scale.value() as f32;
            }
        });
    }

    {
        let draft = draft.clone();
        let tint_values = tint_values.clone();
        let wallpaper_tint_spins = wallpaper_tint_spins.clone();
        let syncing = syncing.clone();
        wallpaper_tint.connect_active_notify(move |toggle| {
            if syncing.get() {
                return;
            }
            tint_values.set_sensitive(toggle.is_active());
            draft.borrow_mut().wallpaper.tint = toggle.is_active().then(|| {
                ThemeColor::srgb(
                    wallpaper_tint_spins[0].value() as f32,
                    wallpaper_tint_spins[1].value() as f32,
                    wallpaper_tint_spins[2].value() as f32,
                    wallpaper_tint_spins[3].value() as f32,
                )
            });
        });
    }

    for spin in &wallpaper_tint_spins {
        let draft = draft.clone();
        let wallpaper_tint_spins = wallpaper_tint_spins.clone();
        let syncing = syncing.clone();
        spin.connect_value_changed(move |_| {
            if syncing.get() || draft.borrow().wallpaper.tint.is_none() {
                return;
            }
            draft.borrow_mut().wallpaper.tint = Some(ThemeColor::srgb(
                wallpaper_tint_spins[0].value() as f32,
                wallpaper_tint_spins[1].value() as f32,
                wallpaper_tint_spins[2].value() as f32,
                wallpaper_tint_spins[3].value() as f32,
            ));
        });
    }

    let sync_semantic_color_controls = {
        let draft = draft.clone();
        let semantic_surface_select = semantic_surface_select.clone();
        let semantic_state_select = semantic_state_select.clone();
        let semantic_override = semantic_override.clone();
        let semantic_color_spins = semantic_color_spins.clone();
        let syncing = syncing.clone();
        Rc::new(move || {
            let draft = draft.borrow();
            let style = semantic_surface(&draft.semantic, semantic_surface_select.selected());
            let state = interaction_state_from_index(semantic_state_select.selected());
            let explicit = match state {
                InteractionState::Normal => true,
                InteractionState::Hover => style.hover.is_some(),
                InteractionState::Pressed => style.pressed.is_some(),
                InteractionState::Selected => style.selected.is_some(),
                InteractionState::Focused => style.focused.is_some(),
                InteractionState::Urgent => style.urgent.is_some(),
                InteractionState::Disabled => style.disabled.is_some(),
            };
            syncing.set(true);
            semantic_override.set_sensitive(state != InteractionState::Normal);
            semantic_override.set_active(explicit);
            for (spin, value) in semantic_color_spins
                .iter()
                .zip(style.resolve(state).components())
            {
                spin.set_value(f64::from(value));
                spin.set_sensitive(explicit);
            }
            syncing.set(false);
        })
    };

    for dropdown in [&semantic_surface_select, &semantic_state_select] {
        let sync = sync_semantic_color_controls.clone();
        let semantic_preview = semantic_preview.clone();
        dropdown.connect_selected_notify(move |_| {
            sync();
            semantic_preview.queue_draw();
        });
    }

    {
        let draft = draft.clone();
        let semantic_surface_select = semantic_surface_select.clone();
        let semantic_state_select = semantic_state_select.clone();
        let semantic_color_spins = semantic_color_spins.clone();
        let semantic_preview = semantic_preview.clone();
        let syncing = syncing.clone();
        semantic_override.connect_active_notify(move |toggle| {
            if syncing.get() {
                return;
            }
            let state = interaction_state_from_index(semantic_state_select.selected());
            let mut draft = draft.borrow_mut();
            let style =
                semantic_surface_mut(&mut draft.semantic, semantic_surface_select.selected());
            if let Some(slot) = style.override_for_mut(state) {
                *slot = toggle.is_active().then(|| {
                    ThemeColor::srgb(
                        semantic_color_spins[0].value() as f32,
                        semantic_color_spins[1].value() as f32,
                        semantic_color_spins[2].value() as f32,
                        semantic_color_spins[3].value() as f32,
                    )
                });
            }
            for spin in &semantic_color_spins {
                spin.set_sensitive(toggle.is_active());
            }
            semantic_preview.queue_draw();
        });
    }

    for spin in &semantic_color_spins {
        let draft = draft.clone();
        let semantic_surface_select = semantic_surface_select.clone();
        let semantic_state_select = semantic_state_select.clone();
        let semantic_color_spins = semantic_color_spins.clone();
        let semantic_preview = semantic_preview.clone();
        let syncing = syncing.clone();
        spin.connect_value_changed(move |_| {
            if syncing.get() {
                return;
            }
            let color = ThemeColor::srgb(
                semantic_color_spins[0].value() as f32,
                semantic_color_spins[1].value() as f32,
                semantic_color_spins[2].value() as f32,
                semantic_color_spins[3].value() as f32,
            );
            let state = interaction_state_from_index(semantic_state_select.selected());
            let mut draft = draft.borrow_mut();
            let style =
                semantic_surface_mut(&mut draft.semantic, semantic_surface_select.selected());
            if state == InteractionState::Normal {
                style.normal = color;
            } else if let Some(slot) = style.override_for_mut(state) {
                *slot = Some(color);
            }
            semantic_preview.queue_draw();
        });
    }

    for (scale, field) in [
        (&semantic_border_width, 0u8),
        (&semantic_radius, 1),
        (&semantic_shadow, 2),
        (&semantic_glow, 3),
        (&semantic_font_weight, 4),
        (&semantic_font_size, 5),
        (&semantic_letter_spacing, 6),
        (&semantic_bar_height, 7),
        (&semantic_dock_width, 8),
        (&semantic_padding, 9),
        (&semantic_gap, 10),
        (&semantic_icon_size, 11),
        (&semantic_sdr_white, 12),
        (&semantic_luminance_cap, 13),
        (&semantic_wallpaper_blur, 14),
        (&semantic_wallpaper_saturation, 15),
    ] {
        let draft = draft.clone();
        let semantic_preview = semantic_preview.clone();
        let syncing = syncing.clone();
        scale.connect_value_changed(move |scale| {
            if syncing.get() {
                return;
            }
            let value = scale.value() as f32;
            let mut draft = draft.borrow_mut();
            match field {
                0 => draft.semantic.edges.border_width = value,
                1 => draft.semantic.edges.radius = value,
                2 => draft.semantic.edges.shadow = value,
                3 => draft.semantic.edges.glow = value,
                4 => draft.semantic.typography.font_weight = value as u16,
                5 => draft.semantic.typography.size = value,
                6 => draft.semantic.typography.letter_spacing = value,
                7 => draft.semantic.layout.bar_height = value,
                8 => draft.semantic.layout.dock_width = value,
                9 => draft.semantic.layout.padding = value,
                10 => draft.semantic.layout.gap = value,
                11 => draft.semantic.layout.icon_size = value,
                12 => {
                    draft.semantic.color_behavior.sdr_white_nits = value;
                    draft.semantic.color_behavior.luminance_cap_nits =
                        draft.semantic.color_behavior.luminance_cap_nits.max(value);
                }
                13 => {
                    draft.semantic.color_behavior.luminance_cap_nits =
                        value.max(draft.semantic.color_behavior.sdr_white_nits);
                }
                14 => draft.semantic.wallpaper.blur = value,
                _ => draft.semantic.wallpaper.saturation = value,
            }
            semantic_preview.queue_draw();
        });
    }

    {
        let draft = draft.clone();
        let syncing = syncing.clone();
        semantic_gamut_mapping.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            draft.borrow_mut().semantic.color_behavior.gamut_mapping = match dropdown.selected() {
                0 => focaldesk_themes::GamutMapping::Clip,
                2 => focaldesk_themes::GamutMapping::PreserveHue,
                _ => focaldesk_themes::GamutMapping::Perceptual,
            };
        });
    }

    {
        let draft = draft.clone();
        let syncing = syncing.clone();
        let semantic_preview = semantic_preview.clone();
        semantic_auto_accent.connect_active_notify(move |toggle| {
            if !syncing.get() {
                draft.borrow_mut().semantic.wallpaper.automatic_accent = toggle.is_active();
                if toggle.is_active() {
                    let path = draft.borrow().wallpaper.path.clone();
                    if let Some(accent) = path
                        .and_then(|path| extract_wallpaper_accent(std::path::Path::new(&path)).ok())
                    {
                        draft.borrow_mut().semantic.surfaces.active_button.normal = accent;
                        semantic_preview.queue_draw();
                    }
                }
            }
        });
    }
    for (button, target) in [
        (&semantic_primary_text, 0u8),
        (&semantic_secondary_text, 1),
        (&semantic_border_color, 2),
        (&semantic_inner_highlight, 3),
    ] {
        let draft = draft.clone();
        let semantic_preview = semantic_preview.clone();
        let syncing = syncing.clone();
        button.connect_rgba_notify(move |button| {
            if syncing.get() {
                return;
            }
            let rgba = button.rgba();
            let color = ThemeColor::srgb(
                srgb_decode(f64::from(rgba.red())) as f32,
                srgb_decode(f64::from(rgba.green())) as f32,
                srgb_decode(f64::from(rgba.blue())) as f32,
                rgba.alpha(),
            );
            match target {
                0 => draft.borrow_mut().semantic.typography.primary = color,
                1 => draft.borrow_mut().semantic.typography.secondary = color,
                2 => draft.borrow_mut().semantic.edges.border_color = color,
                _ => draft.borrow_mut().semantic.edges.inner_highlight = color,
            }
            semantic_preview.queue_draw();
        });
    }
    sync_semantic_color_controls();

    let save_as_action: Rc<dyn Fn()> = {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let current_path = current_path.clone();
        let file_message = file_message.clone();
        Rc::new(move || {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Save FocalDesk Theme");
            dialog.set_initial_name(Some("focaldesk-theme.toml"));
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("FocalDesk themes (*.toml)"));
            filter.add_pattern("*.toml");
            dialog.set_default_filter(Some(&filter));
            dialog.save(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, {
                let draft = draft.clone();
                let saved_document = saved_document.clone();
                let current_path = current_path.clone();
                let file_message = file_message.clone();
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        file_message.set_text("That destination is not a local file");
                        return;
                    };
                    match persist_theme_editor_document(
                        &draft,
                        &saved_document,
                        &current_path,
                        path,
                    ) {
                        Ok(()) => file_message.set_text("Theme saved"),
                        Err(error) => file_message.set_text(&format!("Save failed: {error}")),
                    }
                }
            });
        })
    };

    {
        let draft = draft.clone();
        let package_row = package_row.clone();
        export_package.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export FocalDesk Theme Package");
            dialog.set_initial_name(Some("focaldesk-theme.fdtheme"));
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("FocalDesk theme packages (*.fdtheme)"));
            filter.add_pattern("*.fdtheme");
            dialog.set_default_filter(Some(&filter));
            dialog.save(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, {
                let draft = draft.clone();
                let package_row = package_row.clone();
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        package_row.set_subtitle("Export destination must be local");
                        return;
                    };
                    let result = ThemePackage::from_document(&draft.borrow().document())
                        .and_then(|package| package.save(&theme_package_path(path)));
                    match result {
                        Ok(()) => package_row.set_subtitle("Package exported"),
                        Err(error) => package_row.set_subtitle(&format!("Export failed: {error}")),
                    }
                }
            });
        });
    }

    {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let current_path = current_path.clone();
        let installed_slug = installed_slug.clone();
        let sync_document_ui = sync_document_ui.clone();
        let package_row = package_row.clone();
        let uninstall_package = uninstall_package.clone();
        import_package.connect_clicked(move |_| {
            if draft.borrow().document() != *saved_document.borrow() {
                package_row.set_subtitle("Save or revert unsaved changes before importing");
                return;
            }
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Import FocalDesk Theme Package");
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("FocalDesk theme packages (*.fdtheme)"));
            filter.add_pattern("*.fdtheme");
            dialog.set_default_filter(Some(&filter));
            dialog.open(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, {
                let draft = draft.clone();
                let saved_document = saved_document.clone();
                let current_path = current_path.clone();
                let installed_slug = installed_slug.clone();
                let sync_document_ui = sync_document_ui.clone();
                let package_row = package_row.clone();
                let uninstall_package = uninstall_package.clone();
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        package_row.set_subtitle("Package source must be local");
                        return;
                    };
                    let imported = ThemePackage::load(&path)
                        .and_then(|package| {
                            let installed = package.install(&installed_themes_root())?;
                            let document = ThemeDocument::load(&installed.document_path)?;
                            Ok((installed, document))
                        })
                        .and_then(|(installed, document)| {
                            ThemeEditorDraft::from_document(&document)
                                .map(|loaded| (installed, document, loaded))
                                .map_err(anyhow::Error::msg)
                        });
                    match imported {
                        Ok((installed, document, loaded)) => {
                            *draft.borrow_mut() = loaded;
                            *saved_document.borrow_mut() = document;
                            *current_path.borrow_mut() = Some(installed.document_path);
                            *installed_slug.borrow_mut() = Some(installed.slug.clone());
                            let snapshot = draft.borrow().clone();
                            sync_document_ui(&snapshot);
                            uninstall_package.set_sensitive(true);
                            package_row.set_subtitle(&format!("Installed: {}", installed.slug));
                        }
                        Err(error) => {
                            package_row.set_subtitle(&format!("Import failed: {error}"));
                        }
                    }
                }
            });
        });
    }

    {
        let draft = draft.clone();
        let current_path = current_path.clone();
        let installed_slug = installed_slug.clone();
        let package_row = package_row.clone();
        let uninstall_package = uninstall_package.clone();
        uninstall_package.clone().connect_clicked(move |_| {
            let Some(slug) = installed_slug.borrow().clone() else {
                return;
            };
            match ThemePackage::uninstall(&installed_themes_root(), &slug) {
                Ok(()) => {
                    *installed_slug.borrow_mut() = None;
                    *current_path.borrow_mut() = None;
                    draft.borrow_mut().wallpaper.path = None;
                    uninstall_package.set_sensitive(false);
                    package_row.set_subtitle("Theme uninstalled; editor draft retained");
                }
                Err(error) => package_row.set_subtitle(&format!("Uninstall failed: {error}")),
            }
        });
    }

    {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let current_path = current_path.clone();
        let file_message = file_message.clone();
        let save_as_action = save_as_action.clone();
        save_theme.connect_clicked(move |_| {
            let Some(path) = current_path.borrow().clone() else {
                save_as_action();
                return;
            };
            match persist_theme_editor_document(&draft, &saved_document, &current_path, path) {
                Ok(()) => file_message.set_text("Theme saved"),
                Err(error) => file_message.set_text(&format!("Save failed: {error}")),
            }
        });
    }

    {
        let save_as_action = save_as_action.clone();
        save_theme_as.connect_clicked(move |_| save_as_action());
    }

    {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let sync_document_ui = sync_document_ui.clone();
        let file_message = file_message.clone();
        revert_theme.connect_clicked(move |_| {
            match ThemeEditorDraft::from_document(&saved_document.borrow()) {
                Ok(restored) => {
                    *draft.borrow_mut() = restored;
                    let snapshot = draft.borrow().clone();
                    sync_document_ui(&snapshot);
                    file_message.set_text("Unsaved changes reverted");
                }
                Err(error) => file_message.set_text(&format!("Revert failed: {error}")),
            }
        });
    }

    let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Probe);

    {
        let draft = draft.clone();
        let last_preview_document = last_preview_document.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        let runtime_row = runtime_row.clone();
        apply_runtime.clone().connect_clicked(move |_| {
            let document = draft.borrow().document();
            *last_preview_document.borrow_mut() = Some(document.clone());
            runtime_row.set_subtitle("Applying runtime theme…");
            let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Apply(document));
        });
    }

    {
        let draft = draft.clone();
        let last_preview_document = last_preview_document.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        let runtime_row = runtime_row.clone();
        revert_runtime.clone().connect_clicked(move |_| {
            *last_preview_document.borrow_mut() = Some(draft.borrow().document());
            runtime_row.set_subtitle("Reverting preview…");
            let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Revert);
        });
    }

    {
        let draft = draft.clone();
        let last_preview_document = last_preview_document.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        let runtime_row = runtime_row.clone();
        live_preview.connect_active_notify(move |toggle| {
            *last_preview_document.borrow_mut() = None;
            if toggle.is_active() {
                runtime_row.set_subtitle("Live preview enabled · waiting for changes");
                return;
            }
            *last_preview_document.borrow_mut() = Some(draft.borrow().document());
            runtime_row.set_subtitle("Disabling preview…");
            let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Revert);
        });
    }

    {
        let draft = draft.clone();
        let last_preview_document = last_preview_document.clone();
        let preview_in_flight = preview_in_flight.clone();
        let editor_page_active = editor_page_active.clone();
        let live_preview = live_preview.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        glib::timeout_add_local(Duration::from_millis(225), move || {
            if !editor_page_active.get() || !live_preview.is_active() || preview_in_flight.get() {
                return glib::ControlFlow::Continue;
            }
            let document = draft.borrow().document();
            if last_preview_document.borrow().as_ref() == Some(&document) {
                return glib::ControlFlow::Continue;
            }
            *last_preview_document.borrow_mut() = Some(document.clone());
            preview_in_flight.set(true);
            if theme_ipc_tx
                .send(ThemeEditorIpcJob::Preview(document))
                .is_err()
            {
                preview_in_flight.set(false);
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let theme_ipc_rx = theme_ipc_rx.clone();
        let preview_in_flight = preview_in_flight.clone();
        let compositor_connected = compositor_connected.clone();
        let runtime_row = runtime_row.clone();
        let apply_runtime = apply_runtime.clone();
        let revert_runtime = revert_runtime.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(message) = theme_ipc_rx.borrow().try_recv() {
                if message.action == ThemeEditorIpcAction::Preview {
                    preview_in_flight.set(false);
                }
                match message.result {
                    Ok(status) => {
                        compositor_connected.set(true);
                        runtime_row
                            .set_subtitle(&theme_editor_runtime_label(status, message.gradient));
                        apply_runtime.set_sensitive(true);
                        revert_runtime.set_sensitive(status.preview_active);
                    }
                    Err(error) => {
                        compositor_connected.set(false);
                        let action = match message.action {
                            ThemeEditorIpcAction::Apply => "Apply failed",
                            ThemeEditorIpcAction::Revert => "Revert failed",
                            ThemeEditorIpcAction::Probe | ThemeEditorIpcAction::Preview => {
                                "Disconnected"
                            }
                        };
                        runtime_row.set_subtitle(&format!("{action} · {error}"));
                        apply_runtime.set_sensitive(false);
                        revert_runtime.set_sensitive(false);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let editor_page_active = editor_page_active.clone();
        let last_preview_document = last_preview_document.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        editor_root.connect_map(move |_| {
            editor_page_active.set(true);
            *last_preview_document.borrow_mut() = None;
            let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Probe);
        });
    }

    {
        let editor_page_active = editor_page_active.clone();
        let theme_ipc_tx = theme_ipc_tx.clone();
        editor_root.connect_unmap(move |_| {
            editor_page_active.set(false);
            let _ = theme_ipc_tx.send(ThemeEditorIpcJob::Revert);
        });
    }

    {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let current_path = current_path.clone();
        let installed_slug = installed_slug.clone();
        let sync_document_ui = sync_document_ui.clone();
        let file_message = file_message.clone();
        let uninstall_package = uninstall_package.clone();
        open_theme.connect_clicked(move |_| {
            if draft.borrow().document() != *saved_document.borrow() {
                file_message.set_text("Save or revert unsaved changes before opening a theme");
                return;
            }
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Open FocalDesk Theme");
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("FocalDesk themes (*.toml)"));
            filter.add_pattern("*.toml");
            dialog.set_default_filter(Some(&filter));
            dialog.open(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, {
                let draft = draft.clone();
                let saved_document = saved_document.clone();
                let current_path = current_path.clone();
                let installed_slug = installed_slug.clone();
                let sync_document_ui = sync_document_ui.clone();
                let file_message = file_message.clone();
                let uninstall_package = uninstall_package.clone();
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        file_message.set_text("That source is not a local file");
                        return;
                    };
                    let loaded = ThemeDocument::load(&path)
                        .map_err(|error| error.to_string())
                        .and_then(|document| {
                            ThemeEditorDraft::from_document(&document)
                                .map(|draft| (document, draft))
                        });
                    match loaded {
                        Ok((document, loaded_draft)) => {
                            *draft.borrow_mut() = loaded_draft;
                            *saved_document.borrow_mut() = document;
                            *current_path.borrow_mut() = Some(path);
                            *installed_slug.borrow_mut() = None;
                            uninstall_package.set_sensitive(false);
                            let snapshot = draft.borrow().clone();
                            sync_document_ui(&snapshot);
                            file_message.set_text("Theme loaded");
                        }
                        Err(error) => file_message.set_text(&format!("Open failed: {error}")),
                    }
                }
            });
        });
    }

    {
        let draft = draft.clone();
        let saved_document = saved_document.clone();
        let current_path = current_path.clone();
        let file_row = file_row.clone();
        let save_theme = save_theme.clone();
        let revert_theme = revert_theme.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            let dirty = draft.borrow().document() != *saved_document.borrow();
            save_theme.set_sensitive(dirty);
            revert_theme.set_sensitive(dirty);
            file_row.set_subtitle(&theme_editor_path_label(
                current_path.borrow().as_deref(),
                dirty,
            ));
            glib::ControlFlow::Continue
        });
    }

    {
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let gradient_group = gradient_group.clone();
        let angle_row = angle_row.clone();
        let radial_center_x_row = radial_center_x_row.clone();
        let radial_center_y_row = radial_center_y_row.clone();
        let radial_radius_row = radial_radius_row.clone();
        let remove_stop = remove_stop.clone();
        let sync_numeric = sync_numeric.clone();
        mode.connect_selected_notify(move |dropdown| {
            let selected = dropdown.selected();
            {
                let mut draft = draft.borrow_mut();
                draft.switch_mode(selected);
                sync_numeric(&draft);
            }
            gradient_group.set_visible(selected != 0);
            angle_row.set_visible(selected == 1);
            radial_center_x_row.set_visible(selected == 2);
            radial_center_y_row.set_visible(selected == 2);
            radial_radius_row.set_visible(selected == 2);
            remove_stop.set_sensitive(draft.borrow().stops.len() > 2);
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
        });
    }

    {
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let hdr_luminance = hdr_luminance.clone();
        let preview_range_status = preview_range_status.clone();
        dynamic_range.connect_selected_notify(move |dropdown| {
            let is_hdr = dropdown.selected() == 1;
            draft.borrow_mut().dynamic_range = if is_hdr {
                ThemeDynamicRange::Hdr
            } else {
                ThemeDynamicRange::Sdr
            };
            draft.borrow_mut().semantic.color_behavior.dynamic_range = if is_hdr {
                ThemeDynamicRange::Hdr
            } else {
                ThemeDynamicRange::Sdr
            };
            hdr_luminance.set_sensitive(is_hdr);
            if is_hdr {
                preview_range_status.set_text(&format!(
                    "HDR · {:.0} nits · SDR-mapped preview",
                    draft.borrow().hdr_luminance_nits
                ));
            } else {
                preview_range_status.set_text("SDR preview");
            }
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
        });
    }

    {
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let preview_range_status = preview_range_status.clone();
        hdr_luminance.connect_value_changed(move |scale| {
            draft.borrow_mut().hdr_luminance_nits = scale.value();
            draft
                .borrow_mut()
                .semantic
                .color_behavior
                .luminance_cap_nits = scale.value() as f32;
            preview_range_status.set_text(&format!(
                "HDR · {:.0} nits · SDR-mapped preview",
                scale.value()
            ));
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
        });
    }

    {
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        let syncing = syncing.clone();
        gamut.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let target = if dropdown.selected() == 0 {
                ThemeColorSpace::Srgb
            } else {
                ThemeColorSpace::DisplayP3
            };
            {
                let mut draft = draft.borrow_mut();
                draft.switch_space(target);
                draft.semantic.color_behavior.gamut = target;
                sync_numeric(&draft);
            }
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
        });
    }

    {
        let gesture = gtk::GestureClick::new();
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail_clone = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        gesture.connect_pressed(move |gesture, _, x, _| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let position =
                ((x - 8.0) / (f64::from(widget.width()) - 16.0).max(1.0)).clamp(0.0, 1.0) as f32;
            let selected = nearest_gradient_stop(&draft.borrow().stops, position);
            let Some(selected) = selected else {
                return;
            };
            {
                let mut draft = draft.borrow_mut();
                draft.select_stop(selected);
                sync_numeric(&draft);
            }
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            stop_rail_clone.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
            update_gradient_stop_accessibility(&stop_rail_clone, &draft.borrow());
            stop_rail_clone.grab_focus();
        });
        stop_rail.add_controller(gesture);
    }

    {
        let drag = gtk::GestureDrag::new();
        let origin_x = Rc::new(Cell::new(0.0));
        {
            let origin_x = origin_x.clone();
            drag.connect_drag_begin(move |_, x, _| origin_x.set(x));
        }
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail_clone = stop_rail.clone();
        let stop_position = stop_position.clone();
        let syncing = syncing.clone();
        drag.connect_drag_update(move |gesture, offset_x, _| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let position = ((origin_x.get() + offset_x - 8.0)
                / (f64::from(widget.width()) - 16.0).max(1.0))
            .clamp(0.0, 1.0) as f32;
            draft.borrow_mut().set_selected_stop_position(position);
            syncing.set(true);
            stop_position.set_value(f64::from(position));
            syncing.set(false);
            preview.queue_draw();
            stop_rail_clone.queue_draw();
            update_gradient_stop_accessibility(&stop_rail_clone, &draft.borrow());
        });
        stop_rail.add_controller(drag);
    }

    {
        let keys = gtk::EventControllerKey::new();
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail_clone = stop_rail.clone();
        let stop_position = stop_position.clone();
        let syncing = syncing.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let step = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                0.10
            } else {
                0.01
            };
            let current = draft.borrow().stops[draft.borrow().selected_stop].position;
            let position = match key {
                gtk::gdk::Key::Left | gtk::gdk::Key::Down => (current - step).max(0.0),
                gtk::gdk::Key::Right | gtk::gdk::Key::Up => (current + step).min(1.0),
                gtk::gdk::Key::Home => 0.0,
                gtk::gdk::Key::End => 1.0,
                _ => return glib::Propagation::Proceed,
            };
            draft.borrow_mut().set_selected_stop_position(position);
            syncing.set(true);
            stop_position.set_value(f64::from(position));
            syncing.set(false);
            preview.queue_draw();
            stop_rail_clone.queue_draw();
            update_gradient_stop_accessibility(&stop_rail_clone, &draft.borrow());
            glib::Propagation::Stop
        });
        stop_rail.add_controller(keys);
    }

    for (button, action) in [
        (&add_stop, 0u8),
        (&duplicate_stop, 1u8),
        (&remove_stop, 2u8),
    ] {
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let remove_stop = remove_stop.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        button.connect_clicked(move |_| {
            {
                let mut draft = draft.borrow_mut();
                match action {
                    0 => draft.add_stop(),
                    1 => draft.duplicate_stop(),
                    _ => draft.remove_stop(),
                }
                sync_numeric(&draft);
                remove_stop.set_sensitive(draft.stops.len() > 2);
            }
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
            update_gradient_stop_accessibility(&stop_rail, &draft.borrow());
        });
    }

    {
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let syncing = syncing.clone();
        stop_position.connect_value_changed(move |spin| {
            if syncing.get() {
                return;
            }
            draft
                .borrow_mut()
                .set_selected_stop_position(spin.value() as f32);
            preview.queue_draw();
            stop_rail.queue_draw();
            update_gradient_stop_accessibility(&stop_rail, &draft.borrow());
        });
    }

    {
        let draft = draft.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        interpolation.connect_selected_notify(move |dropdown| {
            draft.borrow_mut().interpolation_space = if dropdown.selected() == 0 {
                ThemeColorSpace::Srgb
            } else {
                ThemeColorSpace::DisplayP3
            };
            preview.queue_draw();
            stop_rail.queue_draw();
        });
    }

    for (spin, field) in [
        (&linear_angle, 0u8),
        (&radial_center_x, 1u8),
        (&radial_center_y, 2u8),
        (&radial_radius, 3u8),
    ] {
        let draft = draft.clone();
        let preview = preview.clone();
        spin.connect_value_changed(move |spin| {
            let mut draft = draft.borrow_mut();
            match field {
                0 => draft.linear_angle = spin.value(),
                1 => draft.radial_center.0 = spin.value(),
                2 => draft.radial_center.1 = spin.value(),
                _ => draft.radial_radius = spin.value(),
            }
            preview.queue_draw();
        });
    }

    {
        let gesture = gtk::GestureClick::new();
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider_clone = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        gesture.connect_pressed(move |gesture, _, x, y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let width = f64::from(widget.width());
            let height = f64::from(widget.height());
            if !point_is_on_hue_ring(width, height, x, y) {
                return;
            }
            {
                let mut draft = draft.borrow_mut();
                draft.hue = hue_from_ring_point(width, height, x, y);
                sync_numeric(&draft);
            }
            picker.queue_draw();
            hue_slider_clone.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider_clone, &draft.borrow());
            hue_slider_clone.grab_focus();
        });
        hue_slider.add_controller(gesture);
    }

    {
        let drag = gtk::GestureDrag::new();
        let origin = Rc::new(Cell::new((0.0, 0.0)));
        let active = Rc::new(Cell::new(false));
        {
            let origin = origin.clone();
            let active = active.clone();
            drag.connect_drag_begin(move |gesture, x, y| {
                let Some(widget) = gesture.widget() else {
                    return;
                };
                origin.set((x, y));
                active.set(point_is_on_hue_ring(
                    f64::from(widget.width()),
                    f64::from(widget.height()),
                    x,
                    y,
                ));
            });
        }
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider_clone = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            if !active.get() {
                return;
            }
            let Some(widget) = gesture.widget() else {
                return;
            };
            let (origin_x, origin_y) = origin.get();
            {
                let mut draft = draft.borrow_mut();
                draft.hue = hue_from_ring_point(
                    f64::from(widget.width()),
                    f64::from(widget.height()),
                    origin_x + offset_x,
                    origin_y + offset_y,
                );
                sync_numeric(&draft);
            }
            picker.queue_draw();
            hue_slider_clone.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider_clone, &draft.borrow());
        });
        hue_slider.add_controller(drag);
    }

    {
        let gesture = gtk::GestureClick::new();
        let draft = draft.clone();
        let picker_clone = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        gesture.connect_pressed(move |gesture, _, x, y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let width = f64::from(widget.width());
            let height = f64::from(widget.height());
            let (saturation, value) = saturation_value_from_point(width, height, x, y);
            {
                let mut draft = draft.borrow_mut();
                draft.saturation = saturation;
                draft.value = value;
                sync_numeric(&draft);
            }
            picker_clone.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker_clone, &hue_slider, &draft.borrow());
            picker_clone.grab_focus();
        });
        picker.add_controller(gesture);
    }

    {
        let drag = gtk::GestureDrag::new();
        let origin = Rc::new(Cell::new((0.0, 0.0)));
        {
            let origin = origin.clone();
            drag.connect_drag_begin(move |_, x, y| origin.set((x, y)));
        }
        let draft = draft.clone();
        let picker_clone = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let (origin_x, origin_y) = origin.get();
            let (saturation, value) = saturation_value_from_point(
                f64::from(widget.width()),
                f64::from(widget.height()),
                origin_x + offset_x,
                origin_y + offset_y,
            );
            {
                let mut draft = draft.borrow_mut();
                draft.saturation = saturation;
                draft.value = value;
                sync_numeric(&draft);
            }
            picker_clone.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker_clone, &hue_slider, &draft.borrow());
        });
        picker.add_controller(drag);
    }

    {
        let keys = gtk::EventControllerKey::new();
        let draft = draft.clone();
        let picker_clone = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let step = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                0.10
            } else {
                0.01
            };
            let changed = {
                let mut draft = draft.borrow_mut();
                match key {
                    gtk::gdk::Key::Left => draft.saturation = (draft.saturation - step).max(0.0),
                    gtk::gdk::Key::Right => draft.saturation = (draft.saturation + step).min(1.0),
                    gtk::gdk::Key::Up => draft.value = (draft.value + step).min(1.0),
                    gtk::gdk::Key::Down => draft.value = (draft.value - step).max(0.0),
                    gtk::gdk::Key::Home => {
                        draft.saturation = 0.0;
                        draft.value = 1.0;
                    }
                    gtk::gdk::Key::End => {
                        draft.saturation = 1.0;
                        draft.value = 0.0;
                    }
                    _ => return glib::Propagation::Proceed,
                }
                sync_numeric(&draft);
                true
            };
            if changed {
                picker_clone.queue_draw();
                preview.queue_draw();
                stop_rail.queue_draw();
                update_theme_editor_status(&gamut_status, &draft.borrow());
                update_theme_editor_accessibility(&picker_clone, &hue_slider, &draft.borrow());
            }
            glib::Propagation::Stop
        });
        picker.add_controller(keys);
    }

    {
        let keys = gtk::EventControllerKey::new();
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider_clone = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let sync_numeric = sync_numeric.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let step = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                10.0
            } else {
                1.0
            };
            {
                let mut draft = draft.borrow_mut();
                match key {
                    gtk::gdk::Key::Left | gtk::gdk::Key::Down => {
                        draft.hue = wrap_hue(draft.hue - step)
                    }
                    gtk::gdk::Key::Right | gtk::gdk::Key::Up => {
                        draft.hue = wrap_hue(draft.hue + step)
                    }
                    gtk::gdk::Key::Home => draft.hue = 0.0,
                    gtk::gdk::Key::End => draft.hue = 359.999,
                    _ => return glib::Propagation::Proceed,
                }
                sync_numeric(&draft);
            }
            picker.queue_draw();
            hue_slider_clone.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider_clone, &draft.borrow());
            glib::Propagation::Stop
        });
        hue_slider.add_controller(keys);
    }

    for spin in &spins {
        let draft = draft.clone();
        let picker = picker.clone();
        let hue_slider = hue_slider.clone();
        let preview = preview.clone();
        let stop_rail = stop_rail.clone();
        let gamut_status = gamut_status.clone();
        let spins = spins.clone();
        let syncing = syncing.clone();
        spin.connect_value_changed(move |_| {
            if syncing.get() {
                return;
            }
            let (hue, saturation, value) =
                rgb_to_hsv([spins[0].value(), spins[1].value(), spins[2].value()]);
            {
                let mut draft = draft.borrow_mut();
                draft.hue = hue;
                draft.saturation = saturation;
                draft.value = value;
                draft.alpha = spins[3].value();
            }
            picker.queue_draw();
            hue_slider.queue_draw();
            preview.queue_draw();
            stop_rail.queue_draw();
            update_theme_editor_status(&gamut_status, &draft.borrow());
            update_theme_editor_accessibility(&picker, &hue_slider, &draft.borrow());
        });
    }

    adw::NavigationPage::new(&editor_root, "Theme Editor")
}

fn appearance_page(
    config: Rc<RefCell<FocalDeskConfig>>,
    settings: Rc<RefCell<Settings>>,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Appearance");

    let visual_group = adw::PreferencesGroup::new();
    visual_group.set_title("Visual Style");

    // Shader chrome
    let shader_row = adw::ActionRow::new();
    shader_row.set_title("Use shader chrome");
    shader_row.set_subtitle("Enable FocalDesk beveled/glass shader styling");

    let shader_switch = gtk::Switch::new();
    shader_switch.set_active(config.borrow().appearance.shader_chrome);

    shader_row.add_suffix(&shader_switch);
    shader_row.set_activatable_widget(Some(&shader_switch));

    {
        let config = config.clone();
        shader_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().appearance.shader_chrome = active;
            persist_config_key(&config.borrow(), "appearance.shader_chrome", json!(active));
        });
    }

    visual_group.add(&shader_row);

    let glass_row = adw::ActionRow::new();
    glass_row.set_title("Work-area glass");
    glass_row.set_subtitle("Add a subtle glass tint and edge highlight behind windows");

    let glass_switch = gtk::Switch::new();
    glass_switch.set_active(config.borrow().appearance.work_area_glass);
    glass_row.add_suffix(&glass_switch);
    glass_row.set_activatable_widget(Some(&glass_switch));

    {
        let config = config.clone();
        glass_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().appearance.work_area_glass = active;
            persist_config_key(
                &config.borrow(),
                "appearance.work_area_glass",
                json!(active),
            );
        });
    }

    visual_group.add(&glass_row);

    // Output focus glow
    let focus_row = adw::ActionRow::new();
    focus_row.set_title("Output focus glow");
    focus_row.set_subtitle("Highlight the currently focused display");

    let focus_switch = gtk::Switch::new();
    focus_switch.set_active(config.borrow().appearance.output_focus_glow);

    focus_row.add_suffix(&focus_switch);
    focus_row.set_activatable_widget(Some(&focus_switch));

    {
        let config = config.clone();
        focus_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().appearance.output_focus_glow = active;
            persist_config_key(
                &config.borrow(),
                "appearance.output_focus_glow",
                json!(active),
            );
        });
    }

    visual_group.add(&focus_row);

    let shelf_visibility = add_dropdown_row(
        &visual_group,
        "Task shelf visibility",
        Some("Choose when the bottom application shelf moves out of the way"),
        TASK_SHELF_VISIBILITY_OPTIONS,
        dock_visibility_index(config.borrow().dock.visibility),
    );
    {
        let config = config.clone();
        shelf_visibility.connect_selected_notify(move |dropdown| {
            let visibility = dock_visibility_from_index(dropdown.selected());
            if config.borrow().dock.visibility == visibility {
                return;
            }
            config.borrow_mut().dock.visibility = visibility;
            let value =
                serde_json::to_value(visibility).unwrap_or_else(|_| json!("intelligent-dodge"));
            persist_config_key(&config.borrow(), "dock.visibility", value);
        });
    }
    page.add(&visual_group);

    let tuning_group = adw::PreferencesGroup::new();
    tuning_group.set_title("Tuning");

    // Glow strength
    let glow_row = adw::ActionRow::new();
    glow_row.set_title("Glow strength");

    let glow_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
    glow_scale.set_hexpand(true);
    glow_scale.set_value(config.borrow().appearance.glow_strength);
    glow_scale.set_draw_value(true);

    let theme_row = adw::ActionRow::new();
    theme_row.set_title("Theme");

    let theme_dropdown = dropdown_from_strings(
        THEME_OPTIONS,
        THEME_OPTIONS
            .iter()
            .position(|theme| *theme == config.borrow().appearance.theme.as_str())
            .unwrap_or(0) as u32,
    );

    {
        let config = config.clone();
        theme_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(theme) = THEME_OPTIONS.get(dropdown.selected() as usize) {
                let theme = (*theme).to_string();
                config.borrow_mut().appearance.theme = theme.clone();
                persist_config_key(&config.borrow(), "appearance.theme", json!(theme));
            }
        });
    }

    theme_row.add_suffix(&theme_dropdown);
    visual_group.add(&theme_row);

    let contrast_row = adw::ActionRow::new();
    contrast_row.set_title("High contrast");
    contrast_row.set_subtitle("Strengthen text, borders, and keyboard focus indicators");
    let contrast_switch = gtk::Switch::new();
    contrast_switch.set_active(settings.borrow().appearance.high_contrast);
    contrast_row.add_suffix(&contrast_switch);
    contrast_row.set_activatable_widget(Some(&contrast_switch));
    {
        let settings = settings.clone();
        contrast_switch.connect_active_notify(move |switch| {
            settings.borrow_mut().appearance.high_contrast = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    visual_group.add(&contrast_row);

    let motion_row = adw::ActionRow::new();
    motion_row.set_title("Reduce motion");
    motion_row.set_subtitle("Disable application and shell interface animations");
    let motion_switch = gtk::Switch::new();
    motion_switch.set_active(!settings.borrow().appearance.animations);
    motion_row.add_suffix(&motion_switch);
    motion_row.set_activatable_widget(Some(&motion_switch));
    {
        let settings = settings.clone();
        motion_switch.connect_active_notify(move |switch| {
            settings.borrow_mut().appearance.animations = !switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    visual_group.add(&motion_row);

    {
        let config = config.clone();
        glow_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            config.borrow_mut().appearance.glow_strength = value;
            persist_config_key(&config.borrow(), "appearance.glow_strength", json!(value));
        });
    }

    glow_row.add_suffix(&glow_scale);
    tuning_group.add(&glow_row);

    // Font scale
    let font_row = adw::ActionRow::new();
    font_row.set_title("Font scale");

    let font_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.75, 1.5, 0.05);
    font_scale.set_hexpand(true);
    font_scale.set_value(config.borrow().appearance.font_scale);
    font_scale.set_draw_value(true);

    {
        let config = config.clone();
        font_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            config.borrow_mut().appearance.font_scale = value;
            persist_config_key(&config.borrow(), "appearance.font_scale", json!(value));
        });
    }

    font_row.add_suffix(&font_scale);
    tuning_group.add(&font_row);

    page.add(&tuning_group);

    let reset_button = gtk::Button::with_label("Reset to Defaults");
    reset_button.add_css_class("destructive-action");
    reset_button.set_halign(gtk::Align::Start);

    {
        let config = config.clone();
        reset_button.connect_clicked(move |_| {
            *config.borrow_mut() = FocalDeskConfig::default();
            persist_config(&config.borrow());
            info!(
                target: "focaldesk",
                session_id = session_id(),
                "reset config"
            );
        });
    }

    let reset_group = adw::PreferencesGroup::new();
    reset_group.add(&reset_button);
    reset_group.set_description(Some("Restore all appearance settings to their defaults"));
    page.add(&reset_group);

    {
        let rx = start_config_watch(&[
            "appearance.shader_chrome",
            "appearance.work_area_glass",
            "appearance.output_focus_glow",
            "appearance.theme",
            "appearance.glow_strength",
            "appearance.font_scale",
            "dock.visibility",
        ]);
        let config = config.clone();
        let shader_switch = shader_switch.clone();
        let glass_switch = glass_switch.clone();
        let focus_switch = focus_switch.clone();
        let theme_dropdown = theme_dropdown.clone();
        let glow_scale = glow_scale.clone();
        let font_scale = font_scale.clone();
        let shelf_visibility = shelf_visibility.clone();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event.key.as_str() {
                    "appearance.shader_chrome" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.shader_chrome = active;
                            set_switch_if_changed(&shader_switch, active);
                        }
                    }
                    "appearance.work_area_glass" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.work_area_glass = active;
                            set_switch_if_changed(&glass_switch, active);
                        }
                    }
                    "appearance.output_focus_glow" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.output_focus_glow = active;
                            set_switch_if_changed(&focus_switch, active);
                        }
                    }
                    "appearance.theme" => {
                        if let Some(theme) = event.value.as_str() {
                            config.borrow_mut().appearance.theme = theme.to_string();
                            if let Some(index) =
                                THEME_OPTIONS.iter().position(|option| *option == theme)
                            {
                                theme_dropdown.set_selected(index as u32);
                            }
                        }
                    }
                    "appearance.glow_strength" => {
                        if let Some(value) = event.value.as_f64() {
                            config.borrow_mut().appearance.glow_strength = value;
                            set_scale_if_changed(&glow_scale, value);
                        }
                    }
                    "appearance.font_scale" => {
                        if let Some(value) = event.value.as_f64() {
                            config.borrow_mut().appearance.font_scale = value;
                            set_scale_if_changed(&font_scale, value);
                        }
                    }
                    "dock.visibility" => {
                        if let Ok(visibility) =
                            serde_json::from_value::<DockVisibility>(event.value.clone())
                        {
                            config.borrow_mut().dock.visibility = visibility;
                            let selected = dock_visibility_index(visibility);
                            if shelf_visibility.selected() != selected {
                                shelf_visibility.set_selected(selected);
                            }
                        }
                    }
                    _ => {}
                }
            }

            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Appearance")
}

fn clear_dynamic_rows(group: &adw::PreferencesGroup, rows: &DynamicRows) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
}

fn add_dynamic_row<W: IsA<gtk::Widget> + Clone + 'static>(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    row: &W,
) {
    group.add(row);
    rows.borrow_mut().push(row.clone().upcast());
}

fn wifi_security_label(security: &str) -> &str {
    if security.trim().is_empty() || security == "--" {
        "Open network"
    } else {
        "Secured network"
    }
}

fn ethernet_connected(device: &EthernetDevice) -> bool {
    matches!(
        device.state.as_str(),
        "connected" | "connecting (getting IP configuration)"
    )
}

fn ethernet_state_label(device: &EthernetDevice) -> String {
    let connection = device
        .connection
        .as_deref()
        .filter(|connection| !connection.is_empty())
        .unwrap_or("No active connection");

    format!("{} | {connection}", device.state)
}

fn populate_ethernet_list(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    snapshot: &EthernetSnapshot,
    status: &StatusBanner,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set(StateKind::ServiceUnavailable, "Network service unavailable");
        status.set_details(Some(err));
    } else if snapshot.devices.is_empty() {
        status.set_details(None);
        status.set_text("No Ethernet devices found");
    } else {
        status.set_details(None);
        status.set_text("Ethernet devices are managed by NetworkManager");
    }

    if snapshot.devices.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No Ethernet devices found");
        row.set_subtitle("Plug in a wired adapter or enable one in NetworkManager");
        add_dynamic_row(group, rows, &row);
        return;
    }

    for device in &snapshot.devices {
        let row = adw::ActionRow::new();
        row.set_title(&device.device);
        row.set_subtitle(&ethernet_state_label(device));
        row.add_prefix(&gtk::Image::from_icon_name("network-wired-symbolic"));

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        if ethernet_connected(device) {
            row.add_suffix(&dim_label("Connected"));

            let disconnect = gtk::Button::with_label("Disconnect");
            disconnect.add_css_class("pill");
            {
                let name = device.device.clone();
                let status = status.clone();
                disconnect.connect_clicked(move |_| {
                    match run_control_command("nmcli", &["device", "disconnect", &name]) {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("Disconnected {name}"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }
            controls.append(&disconnect);
        } else {
            let connect = gtk::Button::with_label("Connect");
            connect.add_css_class("pill");
            {
                let name = device.device.clone();
                let status = status.clone();
                connect.connect_clicked(move |_| {
                    match run_control_command("nmcli", &["device", "connect", &name]) {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("Connected {name}"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }
            controls.append(&connect);
        }

        row.add_suffix(&controls);
        add_dynamic_row(group, rows, &row);
    }
}

fn refresh_ethernet_list_async(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    status: &StatusBanner,
) {
    clear_dynamic_rows(group, rows);
    status.set_text("Loading Ethernet state");

    let row = adw::ActionRow::new();
    row.set_title("Loading wired devices");
    row.set_subtitle("Querying NetworkManager");
    row.add_prefix(&gtk::Image::from_icon_name("network-wired-symbolic"));
    add_dynamic_row(group, rows, &row);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(load_ethernet_snapshot());
    });

    let group = group.clone();
    let rows = rows.clone();
    let status = status.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(snapshot) => {
            populate_ethernet_list(&group, &rows, &snapshot, &status);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Unable to load Ethernet state");
            clear_dynamic_rows(&group, &rows);
            glib::ControlFlow::Break
        }
    });
}

fn populate_wifi_list(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    snapshot: &WifiSnapshot,
    status: &StatusBanner,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set(StateKind::ServiceUnavailable, "Wi-Fi service unavailable");
        status.set_details(Some(err));
    } else if snapshot.enabled {
        status.set_details(None);
        status.set_text("Wi-Fi is enabled");
    } else {
        status.set_details(None);
        status.set_text("Wi-Fi is disabled");
    }

    if snapshot.networks.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(if snapshot.enabled {
            "No Wi-Fi networks found"
        } else {
            "Turn on Wi-Fi to scan for networks"
        });
        add_dynamic_row(group, rows, &row);
        return;
    }

    for network in &snapshot.networks {
        let row = adw::ActionRow::new();
        row.set_title(&network.ssid);
        row.set_subtitle(&format!(
            "{} | Signal {}%",
            wifi_security_label(&network.security),
            network.signal
        ));

        let icon = gtk::Image::from_icon_name(if network.active {
            "network-wireless-acquiring-symbolic"
        } else {
            "network-wireless-symbolic"
        });
        row.add_prefix(&icon);

        if network.active {
            row.add_suffix(&dim_label("Connected"));
        } else {
            let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let password = gtk::PasswordEntry::new();
            password.set_placeholder_text(Some("Password"));
            password.set_width_chars(18);
            password.set_visible(!network.security.trim().is_empty() && network.security != "--");

            let button = gtk::Button::with_label("Connect");
            button.add_css_class("pill");

            {
                let ssid = network.ssid.clone();
                let password = password.clone();
                let status = status.clone();
                button.connect_clicked(move |_| {
                    let password_text = password.text().to_string();
                    let result = if password.is_visible() && !password_text.is_empty() {
                        run_control_command(
                            "nmcli",
                            &[
                                "device",
                                "wifi",
                                "connect",
                                &ssid,
                                "password",
                                &password_text,
                            ],
                        )
                    } else {
                        run_control_command("nmcli", &["device", "wifi", "connect", &ssid])
                    };

                    match result {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("Connected to {ssid}"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }

            controls.append(&password);
            controls.append(&button);
            row.add_suffix(&controls);
        }

        add_dynamic_row(group, rows, &row);
    }
}

fn refresh_wifi_list_async(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    status: &StatusBanner,
    wifi_switch: &gtk::Switch,
    updating_switch: &Rc<Cell<bool>>,
) {
    clear_dynamic_rows(group, rows);
    status.set_text("Loading Wi-Fi state");

    let row = adw::ActionRow::new();
    row.set_title("Loading Wi-Fi networks");
    row.set_subtitle("Querying NetworkManager");
    row.add_prefix(&gtk::Image::from_icon_name("network-wireless-symbolic"));
    add_dynamic_row(group, rows, &row);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(load_wifi_snapshot());
    });

    let group = group.clone();
    let rows = rows.clone();
    let status = status.clone();
    let wifi_switch = wifi_switch.clone();
    let updating_switch = updating_switch.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(snapshot) => {
            updating_switch.set(true);
            wifi_switch.set_active(snapshot.enabled);
            updating_switch.set(false);
            populate_wifi_list(&group, &rows, &snapshot, &status);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Unable to load Wi-Fi state");
            clear_dynamic_rows(&group, &rows);
            glib::ControlFlow::Break
        }
    });
}

fn network_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Network");

    let ethernet_status = StatusBanner::new("Loading Ethernet state");

    let ethernet_group = adw::PreferencesGroup::new();
    ethernet_group.set_title("Ethernet");

    let ethernet_refresh_row = adw::ActionRow::new();
    ethernet_refresh_row.set_title("Refresh Wired Devices");
    ethernet_refresh_row.set_subtitle("Update adapter state and active wired connections");
    let ethernet_refresh_button = gtk::Button::with_label("Refresh");
    ethernet_refresh_button.add_css_class("pill");
    ethernet_refresh_row.add_suffix(&ethernet_refresh_button);
    ethernet_group.add(&ethernet_refresh_row);
    ethernet_group.add(&ethernet_status.widget());
    page.add(&ethernet_group);

    let ethernet_devices_group = adw::PreferencesGroup::new();
    ethernet_devices_group.set_title("Wired Devices");
    page.add(&ethernet_devices_group);
    let ethernet_device_rows = Rc::new(RefCell::new(Vec::new()));

    let wifi_status = StatusBanner::new("Loading Wi-Fi state");

    let controls_group = adw::PreferencesGroup::new();
    controls_group.set_title("Wi-Fi");

    let wifi_row = adw::ActionRow::new();
    wifi_row.set_title("Wi-Fi");
    wifi_row.set_subtitle("Use NetworkManager to scan and connect to wireless networks");
    let wifi_switch = gtk::Switch::new();
    wifi_row.add_suffix(&wifi_switch);
    wifi_row.set_activatable_widget(Some(&wifi_switch));
    controls_group.add(&wifi_row);

    let refresh_row = adw::ActionRow::new();
    refresh_row.set_title("Refresh Networks");
    refresh_row.set_subtitle("Rescan nearby access points");
    let refresh_button = gtk::Button::with_label("Refresh");
    refresh_button.add_css_class("pill");
    refresh_row.add_suffix(&refresh_button);
    controls_group.add(&refresh_row);
    controls_group.add(&wifi_status.widget());
    page.add(&controls_group);

    let networks_group = adw::PreferencesGroup::new();
    networks_group.set_title("Available Wi-Fi Networks");
    page.add(&networks_group);
    let network_rows = Rc::new(RefCell::new(Vec::new()));

    let updating_wifi_switch = Rc::new(Cell::new(false));
    refresh_ethernet_list_async(
        &ethernet_devices_group,
        &ethernet_device_rows,
        &ethernet_status,
    );
    refresh_wifi_list_async(
        &networks_group,
        &network_rows,
        &wifi_status,
        &wifi_switch,
        &updating_wifi_switch,
    );

    {
        let ethernet_devices_group = ethernet_devices_group.clone();
        let ethernet_device_rows = ethernet_device_rows.clone();
        let ethernet_status = ethernet_status.clone();
        ethernet_refresh_button.connect_clicked(move |_| {
            refresh_ethernet_list_async(
                &ethernet_devices_group,
                &ethernet_device_rows,
                &ethernet_status,
            );
        });
    }

    {
        let wifi_status = wifi_status.clone();
        let updating_wifi_switch = updating_wifi_switch.clone();
        wifi_switch.connect_active_notify(move |switch| {
            if updating_wifi_switch.get() {
                return;
            }

            let state = if switch.is_active() { "on" } else { "off" };
            match run_control_command("nmcli", &["radio", "wifi", state]) {
                Ok(_) => wifi_status.set_text(if switch.is_active() {
                    "Wi-Fi enabled"
                } else {
                    "Wi-Fi disabled"
                }),
                Err(err) => wifi_status.set_text(&err),
            }
        });
    }

    {
        let networks_group = networks_group.clone();
        let network_rows = network_rows.clone();
        let wifi_status = wifi_status.clone();
        let wifi_switch = wifi_switch.clone();
        let updating_wifi_switch = updating_wifi_switch.clone();
        refresh_button.connect_clicked(move |_| {
            refresh_wifi_list_async(
                &networks_group,
                &network_rows,
                &wifi_status,
                &wifi_switch,
                &updating_wifi_switch,
            );
        });
    }

    adw::NavigationPage::new(&page, "Network")
}

fn populate_bluetooth_list(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    snapshot: &BluetoothSnapshot,
    status: &StatusBanner,
    scanning: &Rc<RefCell<bool>>,
    power_switch: &gtk::Switch,
    scan_switch: &gtk::Switch,
    updating_switches: &Rc<Cell<bool>>,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set(
            StateKind::ServiceUnavailable,
            "Bluetooth service unavailable",
        );
        status.set_details(Some(err));
    } else if snapshot.powered {
        status.set_details(None);
        status.set_text(if snapshot.scanning {
            "Bluetooth is scanning"
        } else {
            "Bluetooth is powered on"
        });
    } else {
        status.set_details(None);
        status.set_text("Bluetooth is powered off");
    }

    if snapshot.devices.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(if snapshot.powered {
            "No Bluetooth devices found"
        } else {
            "Turn on Bluetooth to manage devices"
        });
        add_dynamic_row(group, rows, &row);
        return;
    }

    for device in &snapshot.devices {
        let row = adw::ActionRow::new();
        row.set_title(&device.name);
        row.set_subtitle(&format!(
            "{}{} | {}",
            if device.connected {
                "Connected"
            } else {
                "Disconnected"
            },
            if device.paired { " | Paired" } else { "" },
            device.address
        ));
        row.add_prefix(&gtk::Image::from_icon_name("bluetooth-symbolic"));

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        if !device.paired {
            let pair = gtk::Button::with_label("Pair");
            pair.add_css_class("pill");
            {
                let address = device.address.clone();
                let pair_button = pair.clone();
                let status = status.clone();
                let group = group.clone();
                let rows = rows.clone();
                let scanning = scanning.clone();
                let power_switch = power_switch.clone();
                let scan_switch = scan_switch.clone();
                let updating_switches = updating_switches.clone();
                pair.connect_clicked(move |_| {
                    pair_button.set_sensitive(false);
                    status.set_text("Pairing and connecting… keep the device in pairing mode");

                    let (tx, rx) = mpsc::channel();
                    let task_address = address.clone();
                    thread::spawn(move || {
                        let _ = tx.send(focaldesk_bluetooth::pair_and_connect(&task_address));
                    });

                    let pair = pair_button.clone();
                    let status = status.clone();
                    let group = group.clone();
                    let rows = rows.clone();
                    let scanning = scanning.clone();
                    let power_switch = power_switch.clone();
                    let scan_switch = scan_switch.clone();
                    let updating_switches = updating_switches.clone();
                    glib::timeout_add_local(Duration::from_millis(50), move || {
                        match rx.try_recv() {
                            Ok(Ok(_)) => {
                                status.set_text("Paired and connected");
                                refresh_bluetooth_list_async(
                                    &group,
                                    &rows,
                                    &status,
                                    &scanning,
                                    &power_switch,
                                    &scan_switch,
                                    &updating_switches,
                                );
                                glib::ControlFlow::Break
                            }
                            Ok(Err(err)) => {
                                pair.set_sensitive(true);
                                status.set_text(&err);
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                pair.set_sensitive(true);
                                status.set_text("Bluetooth pairing task stopped unexpectedly");
                                glib::ControlFlow::Break
                            }
                        }
                    });
                });
            }
            controls.append(&pair);
        } else {
            let connect = gtk::Button::with_label(if device.connected {
                "Disconnect"
            } else {
                "Connect"
            });
            connect.add_css_class("pill");
            {
                let address = device.address.clone();
                let was_connected = device.connected;
                let command = if was_connected {
                    "disconnect"
                } else {
                    "connect"
                };
                let connect_button = connect.clone();
                let status = status.clone();
                let group = group.clone();
                let rows = rows.clone();
                let scanning = scanning.clone();
                let power_switch = power_switch.clone();
                let scan_switch = scan_switch.clone();
                let updating_switches = updating_switches.clone();
                connect.connect_clicked(move |_| {
                    connect_button.set_sensitive(false);
                    status.set_text(if was_connected {
                        "Disconnecting Bluetooth device…"
                    } else {
                        "Connecting Bluetooth device…"
                    });

                    let (tx, rx) = mpsc::channel();
                    let task_address = address.clone();
                    thread::spawn(move || {
                        let result = if was_connected {
                            focaldesk_bluetooth::disconnect(&task_address)
                        } else {
                            focaldesk_bluetooth::connect(&task_address)
                        };
                        let _ = tx.send(result);
                    });

                    let connect = connect_button.clone();
                    let result_address = address.clone();
                    let status = status.clone();
                    let group = group.clone();
                    let rows = rows.clone();
                    let scanning = scanning.clone();
                    let power_switch = power_switch.clone();
                    let scan_switch = scan_switch.clone();
                    let updating_switches = updating_switches.clone();
                    glib::timeout_add_local(Duration::from_millis(50), move || {
                        match rx.try_recv() {
                            Ok(Ok(output)) => {
                                if output.is_empty() {
                                    status.set_text(&format!("{command} sent to {result_address}"));
                                } else {
                                    status.set_text(&output);
                                }
                                refresh_bluetooth_list_async(
                                    &group,
                                    &rows,
                                    &status,
                                    &scanning,
                                    &power_switch,
                                    &scan_switch,
                                    &updating_switches,
                                );
                                glib::ControlFlow::Break
                            }
                            Ok(Err(err)) => {
                                connect.set_sensitive(true);
                                status.set_text(&err);
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                connect.set_sensitive(true);
                                status.set_text("Bluetooth connection task stopped unexpectedly");
                                glib::ControlFlow::Break
                            }
                        }
                    });
                });
            }
            controls.append(&connect);
        }

        if device.paired {
            let trust = gtk::Button::with_label("Trust");
            trust.add_css_class("pill");
            {
                let address = device.address.clone();
                let status = status.clone();
                trust.connect_clicked(move |_| match focaldesk_bluetooth::trust(&address) {
                    Ok(output) if output.is_empty() => {
                        status.set_text(&format!("Trusted {address}"));
                    }
                    Ok(output) => status.set_text(&output),
                    Err(err) => status.set_text(&err),
                });
            }
            controls.append(&trust);

            let remove = gtk::Button::with_label("Remove");
            remove.add_css_class("pill");
            remove.add_css_class("destructive-action");
            {
                let address = device.address.clone();
                let status = status.clone();
                remove.connect_clicked(move |_| match focaldesk_bluetooth::remove(&address) {
                    Ok(_) => {
                        status.set_text(&format!(
                            "Removed {address} — tap Refresh to update the list"
                        ));
                    }
                    Err(err) => status.set_text(&err),
                });
            }
            controls.append(&remove);
        }

        row.add_suffix(&controls);
        add_dynamic_row(group, rows, &row);
    }
}

fn refresh_bluetooth_list_async(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    status: &StatusBanner,
    scanning: &Rc<RefCell<bool>>,
    power_switch: &gtk::Switch,
    scan_switch: &gtk::Switch,
    updating_switches: &Rc<Cell<bool>>,
) {
    clear_dynamic_rows(group, rows);
    status.set_text("Loading Bluetooth state");

    let row = adw::ActionRow::new();
    row.set_title("Loading Bluetooth devices");
    row.set_subtitle("Querying BlueZ");
    row.add_prefix(&gtk::Image::from_icon_name("bluetooth-symbolic"));
    add_dynamic_row(group, rows, &row);

    let scanning_value = *scanning.borrow();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(load_bluetooth_snapshot(scanning_value));
    });

    let group = group.clone();
    let rows = rows.clone();
    let status = status.clone();
    let scanning = scanning.clone();
    let power_switch = power_switch.clone();
    let scan_switch = scan_switch.clone();
    let updating_switches = updating_switches.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(snapshot) => {
            updating_switches.set(true);
            power_switch.set_active(snapshot.powered);
            scan_switch.set_active(snapshot.scanning);
            updating_switches.set(false);
            *scanning.borrow_mut() = snapshot.scanning;
            populate_bluetooth_list(
                &group,
                &rows,
                &snapshot,
                &status,
                &scanning,
                &power_switch,
                &scan_switch,
                &updating_switches,
            );
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Unable to load Bluetooth state");
            clear_dynamic_rows(&group, &rows);
            glib::ControlFlow::Break
        }
    });
}

fn bluetooth_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Bluetooth");

    let scanning = Rc::new(RefCell::new(false));
    let status = StatusBanner::new("Loading Bluetooth state");

    let controls_group = adw::PreferencesGroup::new();
    controls_group.set_title("Bluetooth");

    let power_row = adw::ActionRow::new();
    power_row.set_title("Bluetooth");
    power_row.set_subtitle("Use BlueZ to pair and connect devices");
    let power_switch = gtk::Switch::new();
    power_row.add_suffix(&power_switch);
    power_row.set_activatable_widget(Some(&power_switch));
    controls_group.add(&power_row);

    let scan_row = adw::ActionRow::new();
    scan_row.set_title("Scan for Devices");
    let scan_switch = gtk::Switch::new();
    scan_row.add_suffix(&scan_switch);
    scan_row.set_activatable_widget(Some(&scan_switch));
    controls_group.add(&scan_row);

    let refresh_row = adw::ActionRow::new();
    refresh_row.set_title("Refresh Devices");
    let refresh_button = gtk::Button::with_label("Refresh");
    refresh_button.add_css_class("pill");
    refresh_row.add_suffix(&refresh_button);
    controls_group.add(&refresh_row);
    controls_group.add(&status.widget());
    page.add(&controls_group);

    let devices_group = adw::PreferencesGroup::new();
    devices_group.set_title("Devices");
    page.add(&devices_group);
    let device_rows = Rc::new(RefCell::new(Vec::new()));

    let updating_switches = Rc::new(Cell::new(false));
    refresh_bluetooth_list_async(
        &devices_group,
        &device_rows,
        &status,
        &scanning,
        &power_switch,
        &scan_switch,
        &updating_switches,
    );

    {
        let status = status.clone();
        let updating_switches = updating_switches.clone();
        power_switch.connect_active_notify(move |switch| {
            if updating_switches.get() {
                return;
            }

            match focaldesk_bluetooth::set_power(switch.is_active()) {
                Ok(_) => status.set_text(if switch.is_active() {
                    "Bluetooth powered on"
                } else {
                    "Bluetooth powered off"
                }),
                Err(err) => status.set_text(&err),
            }
        });
    }

    {
        let scanning = scanning.clone();
        let status = status.clone();
        let updating_switches = updating_switches.clone();
        scan_switch.connect_active_notify(move |switch| {
            if updating_switches.get() {
                return;
            }

            match focaldesk_bluetooth::set_scanning(switch.is_active()) {
                Ok(_) => {
                    *scanning.borrow_mut() = switch.is_active();
                    status.set_text(if switch.is_active() {
                        "Bluetooth scan enabled"
                    } else {
                        "Bluetooth scan disabled"
                    });
                }
                Err(err) => status.set_text(&err),
            }
        });
    }

    {
        let devices_group = devices_group.clone();
        let device_rows = device_rows.clone();
        let status = status.clone();
        let scanning = scanning.clone();
        let power_switch = power_switch.clone();
        let scan_switch = scan_switch.clone();
        let updating_switches = updating_switches.clone();
        refresh_button.connect_clicked(move |_| {
            refresh_bluetooth_list_async(
                &devices_group,
                &device_rows,
                &status,
                &scanning,
                &power_switch,
                &scan_switch,
                &updating_switches,
            );
        });
    }

    // Discovery updates BlueZ's device cache asynchronously. Keep the visible
    // list in sync while scanning instead of requiring the user to repeatedly
    // press Refresh after enabling discovery.
    {
        let devices_group = devices_group.clone();
        let device_rows = device_rows.clone();
        let status = status.clone();
        let scanning = scanning.clone();
        let power_switch = power_switch.clone();
        let scan_switch = scan_switch.clone();
        let updating_switches = updating_switches.clone();
        glib::timeout_add_local(Duration::from_secs(2), move || {
            if *scanning.borrow() {
                refresh_bluetooth_list_async(
                    &devices_group,
                    &device_rows,
                    &status,
                    &scanning,
                    &power_switch,
                    &scan_switch,
                    &updating_switches,
                );
            }
            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Bluetooth")
}

fn printer_status_text(printer: &Printer) -> String {
    let enabled = if printer.enabled {
        "Enabled"
    } else {
        "Disabled"
    };
    let accepting = if printer.accepting_jobs {
        "Accepting jobs"
    } else {
        "Rejecting jobs"
    };
    let default = if printer.is_default { " | Default" } else { "" };

    match printer.device_uri.as_deref() {
        Some(uri) if !uri.is_empty() => {
            format!(
                "{enabled} | {accepting} | {} | {uri}{default}",
                printer.state
            )
        }
        _ => format!("{enabled} | {accepting} | {}{default}", printer.state),
    }
}

fn refresh_printer_list(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    snapshot: &PrinterSnapshot,
    status: &gtk::Label,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set_text(err);
    } else if !snapshot.scheduler_running {
        status.set_text("CUPS scheduler is not running");
    } else if snapshot.printers.is_empty() {
        status.set_text("CUPS is running, but no printers are configured");
    } else {
        status.set_text("Printers are managed by CUPS");
    }

    if snapshot.printers.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No Printers Found");
        row.set_subtitle("No CUPS printer queues are configured");
        row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));
        add_dynamic_row(group, rows, &row);
    }

    for printer in &snapshot.printers {
        let row = adw::ActionRow::new();
        row.set_title(&printer.name);
        row.set_subtitle(&printer_status_text(printer));
        row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        if printer.is_default {
            row.add_suffix(&dim_label("Default"));
        } else {
            let set_default = gtk::Button::with_label("Set Default");
            set_default.add_css_class("pill");
            {
                let name = printer.name.clone();
                let status = status.clone();
                set_default.connect_clicked(move |_| {
                    match run_control_command("lpadmin", &["-d", &name]) {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("{name} set as default printer"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }
            controls.append(&set_default);
        }

        let toggle = gtk::Button::with_label(if printer.enabled { "Disable" } else { "Enable" });
        toggle.add_css_class("pill");
        {
            let name = printer.name.clone();
            let command = if printer.enabled {
                "cupsdisable"
            } else {
                "cupsenable"
            };
            let status = status.clone();
            toggle.connect_clicked(move |_| match run_control_command(command, &[&name]) {
                Ok(output) if output.is_empty() => {
                    status.set_text(&format!("{command} sent to {name}"));
                }
                Ok(output) => status.set_text(&output),
                Err(err) => status.set_text(&err),
            });
        }
        controls.append(&toggle);

        let queue = gtk::Button::with_label(if printer.accepting_jobs {
            "Reject Jobs"
        } else {
            "Accept Jobs"
        });
        queue.add_css_class("pill");
        {
            let name = printer.name.clone();
            let command = if printer.accepting_jobs {
                "cupsreject"
            } else {
                "cupsaccept"
            };
            let status = status.clone();
            queue.connect_clicked(move |_| match run_control_command(command, &[&name]) {
                Ok(output) if output.is_empty() => {
                    status.set_text(&format!("{command} sent to {name}"));
                }
                Ok(output) => status.set_text(&output),
                Err(err) => status.set_text(&err),
            });
        }
        controls.append(&queue);

        row.add_suffix(&controls);
        add_dynamic_row(group, rows, &row);
    }
}

fn refresh_installable_printer_list(
    installable_group: &adw::PreferencesGroup,
    installable_rows: &DynamicRows,
    snapshot: &PrinterSnapshot,
    status: &gtk::Label,
    printers_group: &adw::PreferencesGroup,
    printer_rows: &DynamicRows,
) {
    clear_dynamic_rows(installable_group, installable_rows);

    if snapshot.installable_printers.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No Installable Printers Found");
        row.set_subtitle("CUPS did not report any available printer devices");
        row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));
        add_dynamic_row(installable_group, installable_rows, &row);
        return;
    }

    for printer in &snapshot.installable_printers {
        let row = adw::ActionRow::new();
        row.set_title(&printer.suggested_name);
        row.set_subtitle(&format!("{} | {}", printer.kind, printer.uri));
        row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));

        let install_button = gtk::Button::with_label("Install");
        install_button.add_css_class("pill");
        {
            let name = printer.suggested_name.clone();
            let uri = printer.uri.clone();
            let status = status.clone();
            let printers_group = printers_group.clone();
            let printer_rows = printer_rows.clone();
            let installable_group = installable_group.clone();
            let installable_rows = installable_rows.clone();
            install_button.connect_clicked(move |_| {
                match install_printer(&name, &uri, "everywhere") {
                    Ok(output) => {
                        if output.is_empty() {
                            status.set_text(&format!("{name} installed with IPP Everywhere"));
                        } else {
                            status.set_text(&output);
                        }
                        refresh_printer_list_async(
                            &printers_group,
                            &printer_rows,
                            &installable_group,
                            &installable_rows,
                            &status,
                        );
                    }
                    Err(err) => status.set_text(&err),
                }
            });
        }
        row.add_suffix(&install_button);
        add_dynamic_row(installable_group, installable_rows, &row);
    }
}

fn refresh_printer_list_async(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    installable_group: &adw::PreferencesGroup,
    installable_rows: &DynamicRows,
    status: &gtk::Label,
) {
    clear_dynamic_rows(group, rows);
    clear_dynamic_rows(installable_group, installable_rows);
    status.set_text("Loading CUPS state");

    let row = adw::ActionRow::new();
    row.set_title("Loading printers");
    row.set_subtitle("Querying CUPS");
    row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));
    add_dynamic_row(group, rows, &row);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(load_printer_snapshot());
    });

    let group = group.clone();
    let rows = rows.clone();
    let installable_group = installable_group.clone();
    let installable_rows = installable_rows.clone();
    let status = status.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(snapshot) => {
            refresh_printer_list(&group, &rows, &snapshot, &status);
            refresh_installable_printer_list(
                &installable_group,
                &installable_rows,
                &snapshot,
                &status,
                &group,
                &rows,
            );
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Unable to load printer state");
            clear_dynamic_rows(&group, &rows);
            clear_dynamic_rows(&installable_group, &installable_rows);
            glib::ControlFlow::Break
        }
    });
}

fn printers_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Printers");

    let status = dim_label("Loading CUPS state");

    let controls_group = adw::PreferencesGroup::new();
    controls_group.set_title("CUPS");

    let service_row = adw::ActionRow::new();
    service_row.set_title("Print Service");
    service_row.set_subtitle("Use CUPS to manage local and network printers");
    service_row.add_prefix(&gtk::Image::from_icon_name("printer-symbolic"));
    controls_group.add(&service_row);

    let install_group = adw::PreferencesGroup::new();
    install_group.set_title("Install Printer");

    let name_entry = add_entry_row(&install_group, "Name", "Office_Printer");
    let uri_entry = add_entry_row(
        &install_group,
        "Device URI",
        "ipp://printer.local/ipp/print",
    );
    let model_entry = add_entry_row(&install_group, "Driver / Model", "everywhere");

    let install_row = adw::ActionRow::new();
    install_row.set_title("Add Printer");
    install_row.set_subtitle("Install the printer with CUPS using IPP Everywhere by default");
    let install_button = gtk::Button::with_label("Install");
    install_button.add_css_class("pill");
    install_row.add_suffix(&install_button);
    install_group.add(&install_row);

    let refresh_row = adw::ActionRow::new();
    refresh_row.set_title("Refresh Printers");
    refresh_row.set_subtitle("Update printer state, queues, and default destination");
    let refresh_button = gtk::Button::with_label("Refresh");
    refresh_button.add_css_class("pill");
    refresh_row.add_suffix(&refresh_button);
    controls_group.add(&refresh_row);
    controls_group.add(&status);
    page.add(&controls_group);
    page.add(&install_group);

    let printers_group = adw::PreferencesGroup::new();
    printers_group.set_title("Printers");
    page.add(&printers_group);
    let printer_rows = Rc::new(RefCell::new(Vec::new()));

    let installable_group = adw::PreferencesGroup::new();
    installable_group.set_title("Installable Printers");
    page.add(&installable_group);
    let installable_rows = Rc::new(RefCell::new(Vec::new()));

    refresh_printer_list_async(
        &printers_group,
        &printer_rows,
        &installable_group,
        &installable_rows,
        &status,
    );

    {
        let name_entry = name_entry.clone();
        let uri_entry = uri_entry.clone();
        let model_entry = model_entry.clone();
        let status = status.clone();
        let printers_group = printers_group.clone();
        let printer_rows = printer_rows.clone();
        let installable_group = installable_group.clone();
        let installable_rows = installable_rows.clone();
        install_button.connect_clicked(move |_| {
            let name = name_entry.text().to_string();
            let uri = uri_entry.text().to_string();
            let model = model_entry.text().to_string();

            match install_printer(&name, &uri, &model) {
                Ok(output) => {
                    let name = name.trim();
                    if output.is_empty() {
                        status.set_text(&format!("{name} installed"));
                    } else {
                        status.set_text(&output);
                    }
                    refresh_printer_list_async(
                        &printers_group,
                        &printer_rows,
                        &installable_group,
                        &installable_rows,
                        &status,
                    );
                }
                Err(err) => status.set_text(&err),
            }
        });
    }

    {
        let printers_group = printers_group.clone();
        let printer_rows = printer_rows.clone();
        let installable_group = installable_group.clone();
        let installable_rows = installable_rows.clone();
        let status = status.clone();
        refresh_button.connect_clicked(move |_| {
            refresh_printer_list_async(
                &printers_group,
                &printer_rows,
                &installable_group,
                &installable_rows,
                &status,
            );
        });
    }

    adw::NavigationPage::new(&page, "Printers")
}

fn sound_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Sound");

    let output_group = adw::PreferencesGroup::new();
    output_group.set_title("Output");

    let output_device_row = adw::ActionRow::new();
    output_device_row.set_title("Output Device");
    let speaker_icon = gtk::Image::from_icon_name("audio-speakers-symbolic");
    output_device_row.add_prefix(&speaker_icon);
    match load_audio_devices(AudioDeviceKind::Sink) {
        Ok(devices) if devices.is_empty() => {
            output_device_row.add_suffix(&dim_label("No Output Devices"));
        }
        Ok(devices) => {
            let labels: Vec<&str> = devices.iter().map(|device| device.label.as_str()).collect();
            let dropdown = dropdown_from_strings(&labels, 0);
            let devices = Rc::new(devices);
            let row = output_device_row.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                let Some(device) = devices.get(dropdown.selected() as usize) else {
                    return;
                };
                match set_default_audio_device(AudioDeviceKind::Sink, &device.selector) {
                    Ok(()) => row.set_subtitle("Default output changed"),
                    Err(err) => row.set_subtitle(&format!("Could not change output: {err}")),
                }
            });
            output_device_row.add_suffix(&dropdown);
        }
        Err(err) => {
            output_device_row.add_suffix(&dim_label("Output Detection Unavailable"));
            output_device_row.set_subtitle(&err);
        }
    }
    output_group.add(&output_device_row);

    let output_config_row = adw::ActionRow::new();
    output_config_row.set_title("Configuration");
    output_config_row.add_suffix(&dropdown_from_strings(OUTPUT_CONFIGURATION_OPTIONS, 0));
    output_group.add(&output_config_row);

    let output_volume_row = adw::ActionRow::new();
    output_volume_row.set_title("Output Volume");
    let output_volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    output_volume_box.set_hexpand(true);
    let output_volume_icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    let output_volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    output_volume.set_hexpand(true);
    output_volume.set_draw_value(false);
    output_volume.set_value(75.0);
    output_volume_box.append(&output_volume_icon);
    output_volume_box.append(&output_volume);
    output_volume_row.add_suffix(&output_volume_box);
    output_group.add(&output_volume_row);

    let balance_row = adw::ActionRow::new();
    balance_row.set_title("Balance");
    let balance = gtk::Scale::with_range(gtk::Orientation::Horizontal, -1.0, 1.0, 0.05);
    balance.set_hexpand(true);
    balance.set_draw_value(false);
    balance.set_value(0.0);
    balance_row.add_suffix(&balance);
    output_group.add(&balance_row);

    let test_row = adw::ActionRow::new();
    test_row.set_title("Test Speakers");
    let test_button = gtk::Button::with_label("Test...");
    test_button.add_css_class("pill");
    test_button.connect_clicked(|_| play_test_speaker_sound());
    test_row.add_suffix(&test_button);
    output_group.add(&test_row);

    page.add(&output_group);

    let input_group = adw::PreferencesGroup::new();
    input_group.set_title("Input");

    let input_device_row = adw::ActionRow::new();
    input_device_row.set_title("Input Device");
    match load_audio_devices(AudioDeviceKind::Source) {
        Ok(devices) if devices.is_empty() => {
            input_device_row.add_suffix(&dim_label("No Input Devices"));
        }
        Ok(devices) => {
            let labels: Vec<&str> = devices.iter().map(|device| device.label.as_str()).collect();
            let dropdown = dropdown_from_strings(&labels, 0);
            let devices = Rc::new(devices);
            let row = input_device_row.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                let Some(device) = devices.get(dropdown.selected() as usize) else {
                    return;
                };
                match set_default_audio_device(AudioDeviceKind::Source, &device.selector) {
                    Ok(()) => row.set_subtitle("Default input changed"),
                    Err(err) => row.set_subtitle(&format!("Could not change input: {err}")),
                }
            });
            input_device_row.add_suffix(&dropdown);
        }
        Err(err) => {
            input_device_row.add_suffix(&dim_label("Input Detection Unavailable"));
            input_device_row.set_subtitle(&err);
        }
    }
    input_group.add(&input_device_row);

    page.add(&input_group);

    let sounds_group = adw::PreferencesGroup::new();
    sounds_group.set_title("Sounds");

    let volume_levels_row = adw::ActionRow::new();
    volume_levels_row.set_title("Volume Levels");
    volume_levels_row.add_suffix(&suffix_chevron());
    sounds_group.add(&volume_levels_row);

    let alert_sound_row = adw::ActionRow::new();
    alert_sound_row.set_title("Alert Sound");
    alert_sound_row.add_suffix(&dropdown_from_strings(ALERT_SOUND_OPTIONS, 0));
    sounds_group.add(&alert_sound_row);

    page.add(&sounds_group);

    adw::NavigationPage::new(&page, "Sound")
}

fn play_test_speaker_sound() {
    let buffer = SoundBuffer::new(SAMPLE_RATE, 1, generate_ui_sound(UiSound::Success));
    UiSoundPlayer::new().play(&buffer);
}

fn applications_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Applications");

    let defaults_group = adw::PreferencesGroup::new();
    defaults_group.set_title("Default Applications");
    defaults_group.set_description(Some(
        "Commands used by FocalDesk when opening common application targets.",
    ));

    let terminal = add_entry_row(&defaults_group, "Terminal", "alacritty");
    terminal.set_text(&settings.borrow().apps.terminal);
    {
        let settings = settings.clone();
        terminal.connect_changed(move |entry| {
            settings.borrow_mut().apps.terminal = entry.text().to_string();
            persist_settings(&settings.borrow());
        });
    }

    let browser = add_entry_row(&defaults_group, "Web browser", "google-chrome");
    browser.set_text(&settings.borrow().apps.browser);
    {
        let settings = settings.clone();
        browser.connect_changed(move |entry| {
            settings.borrow_mut().apps.browser = entry.text().to_string();
            persist_settings(&settings.borrow());
        });
    }

    let browser_backend = add_dropdown_row(
        &defaults_group,
        "Browser backend",
        Some("Auto launches browsers as Wayland clients; XWayland stays explicit"),
        BROWSER_LAUNCH_BACKEND_OPTIONS,
        browser_launch_backend_index(settings.borrow().apps.browser_launch_backend),
    );
    {
        let settings = settings.clone();
        browser_backend.connect_selected_notify(move |dropdown| {
            settings.borrow_mut().apps.browser_launch_backend =
                selected_browser_launch_backend(dropdown.selected());
            persist_settings(&settings.borrow());
        });
    }

    let file_manager = add_entry_row(&defaults_group, "File manager", "focaldesk-files");
    file_manager.set_text(&settings.borrow().apps.file_manager);
    {
        let settings = settings.clone();
        file_manager.connect_changed(move |entry| {
            settings.borrow_mut().apps.file_manager = entry.text().to_string();
            persist_settings(&settings.borrow());
        });
    }

    let email = add_entry_row(
        &defaults_group,
        "Email",
        "Leave blank to discover Evolution, Thunderbird, or Geary",
    );
    email.set_text(&settings.borrow().apps.email);
    {
        let settings = settings.clone();
        email.connect_changed(move |entry| {
            settings.borrow_mut().apps.email = entry.text().to_string();
            persist_settings(&settings.borrow());
        });
    }

    let pin_email = add_switch_row(
        &defaults_group,
        "Pin email to task shelf",
        Some("Show Email with the other pinned applications when a mail client is available"),
        settings.borrow().apps.pin_email_to_shelf,
    );
    {
        let settings = settings.clone();
        pin_email.connect_active_notify(move |switch| {
            settings.borrow_mut().apps.pin_email_to_shelf = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }

    page.add(&defaults_group);

    let actions_group = adw::PreferencesGroup::new();
    actions_group.set_title("Actions");
    let reset = add_button_row(
        &actions_group,
        "Reset application defaults",
        Some("Restore the built-in FocalDesk application commands"),
        "Reset",
    );
    reset.connect_clicked({
        let settings = settings.clone();
        let terminal = terminal.clone();
        let browser = browser.clone();
        let browser_backend = browser_backend.clone();
        let file_manager = file_manager.clone();
        move |_| {
            settings.borrow_mut().apps = focaldesk_settings_core::default_settings().apps;
            let apps = settings.borrow().apps.clone();
            terminal.set_text(&apps.terminal);
            browser.set_text(&apps.browser);
            browser_backend.set_selected(browser_launch_backend_index(apps.browser_launch_backend));
            file_manager.set_text(&apps.file_manager);
            persist_settings(&settings.borrow());
        }
    });
    page.add(&actions_group);

    adw::NavigationPage::new(&page, "Applications")
}

fn set_chrome_item_visible(hidden: &mut Vec<u32>, id: u32, visible: bool) {
    hidden.retain(|known| *known != id);
    if !visible {
        hidden.push(id);
    }
}

fn parse_chrome_order(text: &str) -> Option<Vec<u32>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    text.split(',')
        .map(|part| part.trim().parse::<u32>().ok())
        .collect()
}

fn chrome_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Chrome");

    let topbar = adw::PreferencesGroup::new();
    topbar.set_title("Top Bar Indicators");
    topbar.set_description(Some(
        "Show or hide live indicators. Changes apply immediately.",
    ));
    for (title, id) in [
        ("Network", 100),
        ("Bluetooth", 101),
        ("Audio", 102),
        ("HDR display status", 103),
        ("Power", 104),
        ("Camera", 105),
        ("Do Not Disturb", 106),
        ("Notifications", 107),
        ("Microphone", 108),
        ("System updates", 109),
    ] {
        let active = !settings.borrow().chrome.topbar.hidden.contains(&id);
        let toggle = add_switch_row(&topbar, title, None, active);
        let settings = settings.clone();
        toggle.connect_active_notify(move |switch| {
            set_chrome_item_visible(
                &mut settings.borrow_mut().chrome.topbar.hidden,
                id,
                switch.is_active(),
            );
            persist_settings(&settings.borrow());
        });
    }
    let topbar_order = add_entry_row(
        &topbar,
        "Indicator order",
        "100, 101, 102, 108, 107, 109, 106, 105, 103, 104",
    );
    topbar_order.set_text(
        &settings
            .borrow()
            .chrome
            .topbar
            .order
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );
    {
        let settings = settings.clone();
        topbar_order.connect_changed(move |entry| {
            let Some(order) = parse_chrome_order(entry.text().as_str()) else {
                return;
            };
            settings.borrow_mut().chrome.topbar.order = order;
            persist_settings(&settings.borrow());
        });
    }
    page.add(&topbar);

    let sidebar = adw::PreferencesGroup::new();
    sidebar.set_title("Sidebar Buttons");
    sidebar.set_description(Some(
        "Workspace controls remain dynamic; fixed application buttons can be shown or hidden.",
    ));
    for (title, id) in [
        ("Settings", 1001),
        ("Launcher", 1000),
        ("Browser", 1005),
        ("Terminal", 1006),
        ("Files", 1007),
    ] {
        let active = !settings.borrow().chrome.sidebar.hidden.contains(&id);
        let toggle = add_switch_row(&sidebar, title, None, active);
        let settings = settings.clone();
        toggle.connect_active_notify(move |switch| {
            set_chrome_item_visible(
                &mut settings.borrow_mut().chrome.sidebar.hidden,
                id,
                switch.is_active(),
            );
            persist_settings(&settings.borrow());
        });
    }
    let sidebar_order = add_entry_row(
        &sidebar,
        "Fixed-item priority",
        "1001, 1000, 1005, 1006, 1007",
    );
    sidebar_order.set_text(
        &settings
            .borrow()
            .chrome
            .sidebar
            .order
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );
    {
        let settings = settings.clone();
        sidebar_order.connect_changed(move |entry| {
            let Some(order) = parse_chrome_order(entry.text().as_str()) else {
                return;
            };
            settings.borrow_mut().chrome.sidebar.order = order;
            persist_settings(&settings.borrow());
        });
    }
    page.add(&sidebar);

    let custom = adw::PreferencesGroup::new();
    custom.set_title("Custom Launch Items");
    custom.set_description(Some(
        "Advanced launch items can be added under chrome.sidebar.custom or chrome.topbar.custom in settings.json. Supported icons include browser, terminal, files, settings, wifi, bluetooth, microphone, speaker, hdr, and power.",
    ));
    add_info_row(
        &custom,
        "Configuration file",
        None,
        &focaldesk_settings_core::settings_path()
            .display()
            .to_string(),
    );
    page.add(&custom);

    adw::NavigationPage::new(&page, "Chrome")
}

fn workspaces_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Workspaces");

    let behavior_group = adw::PreferencesGroup::new();
    behavior_group.set_title("Desktop Behavior");
    behavior_group.set_description(Some(
        "Controls for how workspaces appear, switch, and restore applications.",
    ));

    let count_row = adw::ActionRow::new();
    count_row.set_title("Visible workspace slots");
    count_row.set_subtitle("Maximum numbered workspaces shown before the overflow button");
    let count = gtk::SpinButton::with_range(1.0, 9.0, 1.0);
    count.set_value(settings.borrow().workspaces.max_workspace_slots as f64);
    count.set_numeric(true);
    count_row.add_suffix(&count);
    behavior_group.add(&count_row);
    {
        let settings = settings.clone();
        count.connect_value_changed(move |spin| {
            settings.borrow_mut().workspaces.max_workspace_slots = spin.value() as u32;
            persist_settings(&settings.borrow());
        });
    }

    add_switch_row(
        &behavior_group,
        "Show workspace indicator on top bar",
        Some("Show the active workspace where it is always visible"),
        true,
    );
    add_switch_row(
        &behavior_group,
        "Per-monitor workspaces",
        Some("Keep each display on its own active workspace"),
        false,
    );
    let restore_session = add_switch_row(
        &behavior_group,
        "Restore session",
        Some("Restore apps to their previous workspaces after restart"),
        settings.borrow().workspaces.restore_session,
    );
    {
        let settings = settings.clone();
        restore_session.connect_active_notify(move |switch| {
            settings.borrow_mut().workspaces.restore_session = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let maximize_on_launch = add_switch_row(
        &behavior_group,
        "Maximize app on launch",
        Some(
            "Open new app windows filling the work area; turn off to open at a smaller default size",
        ),
        settings.borrow().workspaces.maximize_on_launch,
    );
    {
        let settings = settings.clone();
        maximize_on_launch.connect_active_notify(move |switch| {
            settings.borrow_mut().workspaces.maximize_on_launch = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    add_switch_row(
        &behavior_group,
        "Wrap around when switching",
        Some("Continue from the last workspace back to the first"),
        true,
    );
    page.add(&behavior_group);

    let keybind_group = adw::PreferencesGroup::new();
    keybind_group.set_title("Keybind Hints");
    add_info_row(&keybind_group, "Switch to workspace 1", None, "Alt+1");
    add_info_row(
        &keybind_group,
        "Move window to workspace 1",
        None,
        "Alt+Shift+1",
    );
    add_info_row(&keybind_group, "Show all workspaces", None, "Alt+0");
    page.add(&keybind_group);

    adw::NavigationPage::new(&page, "Workspaces")
}

fn keyboard_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Keyboard");

    let input_group = adw::PreferencesGroup::new();
    input_group.set_title("Typing");
    add_dropdown_row(
        &input_group,
        "Layout",
        Some("Keyboard layout used for new sessions"),
        KEYBOARD_LAYOUT_OPTIONS,
        0,
    );
    add_scale_row(&input_group, "Repeat delay", 150.0, 1000.0, 25.0, 350.0);
    add_scale_row(&input_group, "Repeat speed", 10.0, 60.0, 1.0, 30.0);
    add_dropdown_row(
        &input_group,
        "Modifier behavior",
        Some("Common modifier remaps"),
        MODIFIER_BEHAVIOR_OPTIONS,
        0,
    );
    page.add(&input_group);

    let shortcuts_group = adw::PreferencesGroup::new();
    shortcuts_group.set_title("Shortcuts");
    shortcuts_group.set_description(Some(
        "Use combinations such as Super+Enter, Ctrl+Alt+D, or Print.",
    ));
    let mut shortcut_entries = Vec::new();
    for &(action, label, default_shortcut) in EDITABLE_KEYBINDINGS {
        let entry = add_entry_row(&shortcuts_group, label, default_shortcut);
        let value = settings
            .borrow()
            .input
            .keybindings
            .get(action)
            .cloned()
            .unwrap_or_else(|| default_shortcut.to_string());
        entry.set_text(&value);
        {
            let settings = settings.clone();
            entry.connect_changed(move |entry| {
                settings
                    .borrow_mut()
                    .input
                    .keybindings
                    .insert(action.to_string(), entry.text().trim().to_string());
                persist_settings(&settings.borrow());
            });
        }
        shortcut_entries.push((action, default_shortcut, entry));
    }

    let reset = add_button_row(
        &shortcuts_group,
        "Reset shortcuts",
        Some("Restore the default keyboard shortcuts"),
        "Reset",
    );
    {
        let settings = settings.clone();
        reset.connect_clicked(move |_| {
            settings.borrow_mut().input.keybindings.clear();
            for &(action, default_shortcut, ref entry) in &shortcut_entries {
                settings
                    .borrow_mut()
                    .input
                    .keybindings
                    .insert(action.to_string(), default_shortcut.to_string());
                entry.set_text(default_shortcut);
            }
            persist_settings(&settings.borrow());
        });
    }
    page.add(&shortcuts_group);

    adw::NavigationPage::new(&page, "Keyboard")
}

fn privacy_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Privacy");

    let permissions_group = adw::PreferencesGroup::new();
    permissions_group.set_title("Permissions");
    add_info_row(
        &permissions_group,
        "Screen sharing",
        Some("The portal asks you to choose a screen for every request"),
        "Ask each time",
    );
    add_info_row(
        &permissions_group,
        "Microphone portal",
        Some("Per-app portal controls are not available in this build"),
        "Unavailable",
    );
    add_info_row(
        &permissions_group,
        "Camera portal",
        Some("Per-app portal controls are not available in this build"),
        "Unavailable",
    );
    let location_services = add_switch_row(
        &permissions_group,
        "Location services",
        Some("Allow location-aware apps to request location access"),
        settings.borrow().privacy.location_services,
    );
    {
        let settings = settings.clone();
        location_services.connect_active_notify(move |switch| {
            settings.borrow_mut().privacy.location_services = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    page.add(&permissions_group);

    let saved_permissions_group = adw::PreferencesGroup::new();
    saved_permissions_group.set_title("Saved App Permissions");
    saved_permissions_group
        .set_description(Some("Persistent permission decisions can be revoked here"));
    populate_saved_permissions(&saved_permissions_group);
    page.add(&saved_permissions_group);

    let history_group = adw::PreferencesGroup::new();
    history_group.set_title("History");
    let recent_files = add_switch_row(
        &history_group,
        "Recent files",
        Some("Allow apps to show recently opened files"),
        settings.borrow().privacy.recent_files,
    );
    {
        let settings = settings.clone();
        recent_files.connect_active_notify(move |switch| {
            settings.borrow_mut().privacy.recent_files = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let history_status = dim_label("Recent history is local to this user");
    let retention = add_dropdown_row(
        &history_group,
        "Notification history retention",
        Some("Maximum saved notification entries"),
        &["25 entries", "50 entries", "100 entries"],
        match settings.borrow().privacy.notification_history_limit {
            0..=25 => 0,
            26..=50 => 1,
            _ => 2,
        },
    );
    {
        let settings = settings.clone();
        retention.connect_selected_notify(move |dropdown| {
            let limit = match dropdown.selected() {
                0 => 25,
                1 => 50,
                _ => 100,
            };
            settings.borrow_mut().privacy.notification_history_limit = limit;
            persist_settings(&settings.borrow());
            let _ = send_notification_request(&NotificationIpcRequest::SetHistoryLimit { limit });
        });
    }
    let clear_on_logout = add_switch_row(
        &history_group,
        "Clear notifications on logout",
        Some("Do not retain notification history between sessions"),
        settings
            .borrow()
            .privacy
            .clear_notification_history_on_logout,
    );
    {
        let settings = settings.clone();
        clear_on_logout.connect_active_notify(move |switch| {
            settings
                .borrow_mut()
                .privacy
                .clear_notification_history_on_logout = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let clear_history = add_button_row(
        &history_group,
        "Clear recent history",
        Some("Remove saved recent file entries"),
        "Clear",
    );
    clear_history.connect_clicked({
        let history_status = history_status.clone();
        move |_| history_status.set_text("Recent history cleared")
    });
    history_group.add(&history_status);
    page.add(&history_group);

    let lock_group = adw::PreferencesGroup::new();
    lock_group.set_title("Lock Screen");
    let hide_lock_screen_notifications = add_switch_row(
        &lock_group,
        "Hide notifications on lock screen",
        Some("Keep notification content private while locked"),
        settings.borrow().privacy.hide_lock_screen_notifications,
    );
    {
        let settings = settings.clone();
        hide_lock_screen_notifications.connect_active_notify(move |switch| {
            settings.borrow_mut().privacy.hide_lock_screen_notifications = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    page.add(&lock_group);

    adw::NavigationPage::new(&page, "Privacy")
}

fn populate_saved_permissions(group: &adw::PreferencesGroup) {
    let mut record_count = 0usize;
    let mut error_count = 0usize;

    match list_ai_permission_records() {
        Ok(records) => {
            record_count += records.len();
            for record in records {
                add_saved_ai_permission_row(group, record);
            }
        }
        Err(err) => {
            error_count += 1;
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "failed to load saved AI permissions"
            );
            add_info_row(
                group,
                "AI permissions could not be loaded",
                Some("Check the FocalDesk log for details"),
                "Error",
            );
        }
    }

    match list_location_permission_records() {
        Ok(records) => {
            record_count += records.len();
            for record in records {
                add_saved_location_permission_row(group, record);
            }
        }
        Err(err) => {
            error_count += 1;
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "failed to load saved location permissions"
            );
            add_info_row(
                group,
                "Location permissions could not be loaded",
                Some("The XDG permission store is unavailable"),
                "Error",
            );
        }
    }

    if record_count == 0 && error_count == 0 {
        add_info_row(
            group,
            "No saved permissions",
            Some("Apps will appear after you save a permission decision"),
            "",
        );
    }
}

fn add_saved_ai_permission_row(group: &adw::PreferencesGroup, record: AiPermissionRecord) {
    let row = adw::ActionRow::new();
    row.set_title(&saved_permission_title(&record));
    row.set_subtitle(&saved_permission_subtitle(&record));

    let revoke = gtk::Button::with_label("Revoke");
    revoke.add_css_class("pill");
    row.add_suffix(&revoke);
    group.add(&row);

    revoke.connect_clicked(move |button| match revoke_ai_permission(&record) {
        Ok(()) => {
            button.set_label("Revoked");
            button.set_sensitive(false);
            row.set_subtitle("This saved decision has been removed");
        }
        Err(err) => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "failed to revoke saved permission"
            );
            button.set_label("Retry");
            row.set_subtitle("Could not revoke this permission; check the FocalDesk log");
        }
    });
}

fn add_saved_location_permission_row(
    group: &adw::PreferencesGroup,
    record: LocationPermissionRecord,
) {
    let row = adw::ActionRow::new();
    row.set_title(&format!("Location — {}", record.decision_label()));
    row.set_subtitle(&format!(
        "{} • {} • XDG portal",
        record.app_id,
        record.accuracy_label()
    ));

    let revoke = gtk::Button::with_label("Revoke");
    revoke.add_css_class("pill");
    row.add_suffix(&revoke);
    group.add(&row);

    revoke.connect_clicked(move |button| match revoke_location_permission(&record) {
        Ok(()) => {
            button.set_label("Revoked");
            button.set_sensitive(false);
            row.set_subtitle("This app will be asked again on its next location request");
        }
        Err(err) => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                app_id = %record.app_id,
                "failed to revoke saved location permission"
            );
            button.set_label("Retry");
            row.set_subtitle("Could not revoke this permission; check the FocalDesk log");
        }
    });
}

fn saved_permission_title(record: &AiPermissionRecord) -> String {
    format!(
        "{} — {}",
        permission_resource_label(record.resource),
        permission_decision_label(record.decision)
    )
}

fn saved_permission_subtitle(record: &AiPermissionRecord) -> String {
    format!(
        "{} • {} • {}",
        record.app_identity,
        permission_target_label(&record.target),
        permission_scope_label(record.scope)
    )
}

fn permission_decision_label(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "Allowed",
        PermissionDecision::Deny => "Denied",
        PermissionDecision::Ask => "Ask",
    }
}

fn permission_scope_label(scope: PermissionScope) -> &'static str {
    match scope {
        PermissionScope::Once => "Once",
        PermissionScope::Session => "Session",
        PermissionScope::Persistent => "Persistent",
    }
}

fn permission_target_label(target: &PermissionTarget) -> String {
    match target {
        PermissionTarget::Global => "Global".to_string(),
        PermissionTarget::Named(name) => name.clone(),
    }
}

fn permission_resource_label(resource: PermissionResource) -> &'static str {
    match resource {
        PermissionResource::Screenshot => "Screenshot",
        PermissionResource::Screencast => "Screencast",
        PermissionResource::ScreenShareWindow => "Window share",
        PermissionResource::ScreenShareOutput => "Output share",
        PermissionResource::AiChat => "AI chat",
        PermissionResource::Microphone => "Microphone",
        PermissionResource::Camera => "Camera",
        PermissionResource::ClipboardRead => "Clipboard read",
        PermissionResource::ClipboardWrite => "Clipboard write",
        PermissionResource::RemoteInput => "Remote input",
        PermissionResource::Notifications => "Notifications",
        PermissionResource::FileOpen => "File open",
        PermissionResource::FileSave => "File save",
    }
}

fn option_index(values: &[Option<u32>], value: Option<u32>) -> u32 {
    values
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0) as u32
}

fn power_button_action_index(action: PowerButtonAction) -> u32 {
    match action {
        PowerButtonAction::ShowPowerMenu => 0,
        PowerButtonAction::Suspend => 1,
        PowerButtonAction::PowerOff => 2,
        PowerButtonAction::DoNothing => 3,
    }
}

fn lid_close_action_index(action: LidCloseAction) -> u32 {
    match action {
        LidCloseAction::Suspend => 0,
        LidCloseAction::BlankScreen => 1,
        LidCloseAction::LockScreen => 2,
        LidCloseAction::DoNothing => 3,
    }
}

fn low_battery_action_index(action: LowBatteryAction) -> u32 {
    match action {
        LowBatteryAction::NotifyOnly => 0,
        LowBatteryAction::Suspend => 1,
        LowBatteryAction::Hibernate => 2,
        LowBatteryAction::PowerOff => 3,
    }
}

fn performance_mode_index(mode: PerformanceMode) -> u32 {
    match mode {
        PerformanceMode::Balanced => 0,
        PerformanceMode::Performance => 1,
        PerformanceMode::PowerSaver => 2,
    }
}

fn browser_launch_backend_index(backend: BrowserLaunchBackend) -> u32 {
    match backend {
        BrowserLaunchBackend::Auto => 0,
        BrowserLaunchBackend::Wayland => 1,
        BrowserLaunchBackend::Xwayland => 2,
    }
}

fn debug_log_level_index(level: DebugLogLevel) -> u32 {
    match level {
        DebugLogLevel::Error => 0,
        DebugLogLevel::Warn => 1,
        DebugLogLevel::Info => 2,
        DebugLogLevel::Debug => 3,
        DebugLogLevel::Trace => 4,
    }
}

fn selected_power_button_action(index: u32) -> PowerButtonAction {
    match index {
        1 => PowerButtonAction::Suspend,
        2 => PowerButtonAction::PowerOff,
        3 => PowerButtonAction::DoNothing,
        _ => PowerButtonAction::ShowPowerMenu,
    }
}

fn selected_lid_close_action(index: u32) -> LidCloseAction {
    match index {
        1 => LidCloseAction::BlankScreen,
        2 => LidCloseAction::LockScreen,
        3 => LidCloseAction::DoNothing,
        _ => LidCloseAction::Suspend,
    }
}

fn selected_low_battery_action(index: u32) -> LowBatteryAction {
    match index {
        1 => LowBatteryAction::Suspend,
        2 => LowBatteryAction::Hibernate,
        3 => LowBatteryAction::PowerOff,
        _ => LowBatteryAction::NotifyOnly,
    }
}

fn selected_browser_launch_backend(index: u32) -> BrowserLaunchBackend {
    match index {
        1 => BrowserLaunchBackend::Wayland,
        2 => BrowserLaunchBackend::Xwayland,
        _ => BrowserLaunchBackend::Auto,
    }
}

fn selected_performance_mode(index: u32) -> PerformanceMode {
    match index {
        1 => PerformanceMode::Performance,
        2 => PerformanceMode::PowerSaver,
        _ => PerformanceMode::Balanced,
    }
}

fn selected_debug_log_level(index: u32) -> DebugLogLevel {
    match index {
        0 => DebugLogLevel::Error,
        1 => DebugLogLevel::Warn,
        3 => DebugLogLevel::Debug,
        4 => DebugLogLevel::Trace,
        _ => DebugLogLevel::Info,
    }
}

fn performance_profile_name(mode: PerformanceMode) -> &'static str {
    match mode {
        PerformanceMode::Balanced => "balanced",
        PerformanceMode::Performance => "performance",
        PerformanceMode::PowerSaver => "power-saver",
    }
}

fn power_status_text(snapshot: &focaldesk_power::PowerSnapshot) -> String {
    let battery = snapshot
        .batteries
        .first()
        .map(
            |battery| match (battery.percentage, battery.state.as_deref()) {
                (Some(percent), Some(state)) => format!("Battery {percent}% · {state}"),
                (Some(percent), None) => format!("Battery {percent}%"),
                (None, Some(state)) => format!("Battery information unavailable · {state}"),
                (None, None) => "Battery information unavailable".to_string(),
            },
        )
        .unwrap_or_else(|| "No battery detected".to_string());
    let mut parts = vec![battery];
    if let Some(online) = snapshot.line_power_online {
        parts.push(if online { "Plugged in" } else { "On battery" }.to_string());
    }
    if let Some(profile) = snapshot
        .performance_profile
        .as_deref()
        .filter(|profile| !profile.is_empty())
    {
        parts.push(format!("{} profile", profile.replace('-', " ")));
    }
    parts.join(" · ")
}

fn run_power_action(request: PowerIpcRequest, status: &StatusBanner) {
    match send_power_request(&request) {
        Ok(PowerIpcResponse::Ok) => status.set_text("Power action started"),
        Ok(PowerIpcResponse::Error { message }) => {
            error!(
                target: "focaldesk",
                session_id = session_id(),
                message = %message,
                "power action failed"
            );
            status.set_text("Power action failed");
        }
        Ok(other) => {
            error!(
                target: "focaldesk",
                session_id = session_id(),
                response = ?other,
                "unexpected power action response"
            );
            status.set_text("Power action failed");
        }
        Err(err) => {
            error!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "power action failed"
            );
            status.set_text("Power action failed");
        }
    }
}

fn power_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Power");
    let snapshot = send_power_request(&PowerIpcRequest::GetSnapshot)
        .ok()
        .and_then(|response| match response {
            PowerIpcResponse::PowerSnapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .unwrap_or_else(|| focaldesk_power::PowerManager::new().snapshot());

    let status_group = adw::PreferencesGroup::new();
    status_group.set_title("Status");
    let status_label = StatusBanner::new(&power_status_text(&snapshot));
    status_group.add(&status_label.widget());
    page.add(&status_group);

    let timing_group = adw::PreferencesGroup::new();
    timing_group.set_title("Idle");
    let blank_screen_dropdown = add_dropdown_row(
        &timing_group,
        "Blank screen after",
        Some("Turn off display output after inactivity"),
        POWER_TIMEOUT_OPTIONS,
        option_index(
            POWER_TIMEOUT_VALUES,
            settings.borrow().power.blank_screen_minutes,
        ),
    );
    {
        let settings = settings.clone();
        blank_screen_dropdown.connect_selected_notify(move |dropdown| {
            let value = POWER_TIMEOUT_VALUES
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or(None);
            settings.borrow_mut().power.blank_screen_minutes = value;
            persist_settings(&settings.borrow());
        });
    }

    let suspend_dropdown = add_dropdown_row(
        &timing_group,
        "Suspend after",
        Some("Suspend the session after longer inactivity"),
        SUSPEND_TIMEOUT_OPTIONS,
        option_index(
            SUSPEND_TIMEOUT_VALUES,
            settings.borrow().power.suspend_minutes,
        ),
    );
    {
        let settings = settings.clone();
        suspend_dropdown.connect_selected_notify(move |dropdown| {
            let value = SUSPEND_TIMEOUT_VALUES
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or(None);
            settings.borrow_mut().power.suspend_minutes = value;
            persist_settings(&settings.borrow());
        });
    }
    page.add(&timing_group);

    let actions_group = adw::PreferencesGroup::new();
    actions_group.set_title("Actions");
    let power_button_dropdown = add_dropdown_row(
        &actions_group,
        "Power button action",
        None,
        POWER_BUTTON_OPTIONS,
        power_button_action_index(settings.borrow().power.power_button_action),
    );
    {
        let settings = settings.clone();
        power_button_dropdown.connect_selected_notify(move |dropdown| {
            settings.borrow_mut().power.power_button_action =
                selected_power_button_action(dropdown.selected());
            persist_settings(&settings.borrow());
        });
    }

    let lid_dropdown = add_dropdown_row(
        &actions_group,
        "Lid close action",
        Some("Shown when laptop lid detection is available"),
        LID_CLOSE_OPTIONS,
        lid_close_action_index(settings.borrow().power.lid_close_action),
    );
    {
        let settings = settings.clone();
        lid_dropdown.connect_selected_notify(move |dropdown| {
            settings.borrow_mut().power.lid_close_action =
                selected_lid_close_action(dropdown.selected());
            persist_settings(&settings.borrow());
        });
    }

    let low_battery_dropdown = add_dropdown_row(
        &actions_group,
        "Low battery action",
        Some("Action for critically low battery"),
        LOW_BATTERY_OPTIONS,
        low_battery_action_index(settings.borrow().power.low_battery_action),
    );
    {
        let settings = settings.clone();
        low_battery_dropdown.connect_selected_notify(move |dropdown| {
            settings.borrow_mut().power.low_battery_action =
                selected_low_battery_action(dropdown.selected());
            persist_settings(&settings.borrow());
        });
    }

    let performance_dropdown = add_dropdown_row(
        &actions_group,
        "Performance mode",
        Some("Uses power-profiles-daemon when available"),
        PERFORMANCE_MODE_OPTIONS,
        performance_mode_index(settings.borrow().power.performance_mode),
    );
    {
        let settings = settings.clone();
        let status_label = status_label.clone();
        performance_dropdown.connect_selected_notify(move |dropdown| {
            let mode = selected_performance_mode(dropdown.selected());
            settings.borrow_mut().power.performance_mode = mode;
            persist_settings(&settings.borrow());

            run_power_action(
                PowerIpcRequest::SetPerformanceProfile {
                    profile: performance_profile_name(mode).to_string(),
                },
                &status_label,
            );
        });
    }
    page.add(&actions_group);

    let system_group = adw::PreferencesGroup::new();
    system_group.set_title("System");
    let suspend_button = add_button_row(&system_group, "Suspend now", None, "Suspend");
    {
        let status_label = status_label.clone();
        suspend_button.connect_clicked(move |_| {
            run_power_action(PowerIpcRequest::Suspend, &status_label);
        });
    }
    let hibernate_button = add_button_row(&system_group, "Hibernate now", None, "Hibernate");
    {
        let status_label = status_label.clone();
        hibernate_button.connect_clicked(move |_| {
            run_power_action(PowerIpcRequest::Hibernate, &status_label);
        });
    }
    let restart_button = add_button_row(&system_group, "Restart", None, "Restart");
    {
        let status_label = status_label.clone();
        restart_button.connect_clicked(move |_| {
            run_power_action(PowerIpcRequest::Reboot, &status_label);
        });
    }
    let poweroff_button = add_button_row(&system_group, "Power off", None, "Power off");
    {
        let status_label = status_label.clone();
        poweroff_button.connect_clicked(move |_| {
            run_power_action(PowerIpcRequest::PowerOff, &status_label);
        });
    }
    page.add(&system_group);

    adw::NavigationPage::new(&page, "Power")
}

fn debug_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Debug");

    let logging_group = adw::PreferencesGroup::new();
    logging_group.set_title("Diagnostics");
    let log_level = add_dropdown_row(
        &logging_group,
        "Log level",
        Some("Runtime logging detail for the desktop session"),
        LOG_LEVEL_OPTIONS,
        debug_log_level_index(settings.borrow().debug.log_level),
    );
    {
        let settings = settings.clone();
        log_level.connect_selected_notify(move |dropdown| {
            settings.borrow_mut().debug.log_level = selected_debug_log_level(dropdown.selected());
            persist_settings(&settings.borrow());
        });
    }
    let show_fps = add_switch_row(
        &logging_group,
        "Log FPS / frame timing",
        Some("Log frame pacing information"),
        settings.borrow().debug.show_fps,
    );
    {
        let settings = settings.clone();
        show_fps.connect_active_notify(move |switch| {
            settings.borrow_mut().debug.show_fps = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let show_damage_regions = add_switch_row(
        &logging_group,
        "Log damage regions",
        Some("Log compositor damage region statistics"),
        settings.borrow().debug.show_damage_regions,
    );
    {
        let settings = settings.clone();
        show_damage_regions.connect_active_notify(move |switch| {
            settings.borrow_mut().debug.show_damage_regions = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let show_input_events = add_switch_row(
        &logging_group,
        "Log input events",
        Some("Display pointer and keyboard event traces"),
        settings.borrow().debug.show_input_events,
    );
    {
        let settings = settings.clone();
        show_input_events.connect_active_notify(move |switch| {
            settings.borrow_mut().debug.show_input_events = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    let verbose_protocol_logs = add_switch_row(
        &logging_group,
        "Enable verbose Wayland/XWayland logs",
        Some("Enable extra protocol logging where available"),
        settings.borrow().debug.verbose_protocol_logs,
    );
    {
        let settings = settings.clone();
        verbose_protocol_logs.connect_active_notify(move |switch| {
            settings.borrow_mut().debug.verbose_protocol_logs = switch.is_active();
            persist_settings(&settings.borrow());
        });
    }
    page.add(&logging_group);

    let files_group = adw::PreferencesGroup::new();
    files_group.set_title("Files");
    let debug_status = dim_label("Diagnostics are generated locally");
    let open_log = add_button_row(
        &files_group,
        "Open log file",
        Some("Open the current FocalDesk session log"),
        "Open",
    );
    open_log.connect_clicked({
        let debug_status = debug_status.clone();
        move |_| {
            if let Some(path) = existing_focaldesk_log_path() {
                match open_path(&path) {
                    Ok(()) => debug_status.set_text("Opened log file"),
                    Err(err) => {
                        warn!(
                            target: "focaldesk",
                            session_id = session_id(),
                            error = %err,
                            "failed to open log file"
                        );
                        debug_status.set_text("Unable to open log file");
                    }
                }
            } else {
                debug_status.set_text("No FocalDesk log file was found");
            }
        }
    });
    let copy_diagnostics = add_button_row(
        &files_group,
        "Copy diagnostics",
        Some("Copy version and session details for bug reports"),
        "Copy",
    );
    copy_diagnostics.connect_clicked({
        let debug_status = debug_status.clone();
        move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&diagnostics_text());
                debug_status.set_text("Diagnostics copied to clipboard");
            } else {
                debug_status.set_text("Clipboard is unavailable");
            }
        }
    });
    files_group.add(&debug_status);
    page.add(&files_group);

    adw::NavigationPage::new(&page, "Debug")
}

fn about_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("About");

    let identity_group = adw::PreferencesGroup::new();
    identity_group.set_title("FocalDesk");
    identity_group.set_description(Some("Early alpha desktop shell and settings app"));

    let brand_row = adw::ActionRow::new();
    brand_row.set_title("FocalDesk");
    brand_row.set_subtitle("A Rust desktop environment built on Smithay and GTK4");
    brand_row.add_prefix(&gtk::Image::from_icon_name("preferences-desktop-symbolic"));
    identity_group.add(&brand_row);
    add_info_row(
        &identity_group,
        "Version",
        Some("Application package version"),
        env!("CARGO_PKG_VERSION"),
    );
    add_info_row(
        &identity_group,
        "Build hash",
        Some("Git commit captured by the build, when available"),
        option_env!("VERGEN_GIT_SHA").unwrap_or("development"),
    );
    add_info_row(&identity_group, "Status", None, "Early alpha");
    page.add(&identity_group);

    let session_group = adw::PreferencesGroup::new();
    session_group.set_title("Session");
    let session_type = session_type_label();
    let current_exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    add_info_row(&session_group, "Session type", None, &session_type);
    add_info_row(
        &session_group,
        "Build profile",
        None,
        if cfg!(debug_assertions) {
            "Debug"
        } else {
            "Release"
        },
    );
    add_info_row(&session_group, "Executable", None, &current_exe);
    page.add(&session_group);

    let project_group = adw::PreferencesGroup::new();
    project_group.set_title("Project");
    add_info_row(&project_group, "License", None, "See LICENSE");

    let github_row = adw::ActionRow::new();
    github_row.set_title("GitHub");
    github_row.set_subtitle("Source code and issue tracking");
    let github = gtk::LinkButton::with_label("https://github.com/sjweiler/focaldesk", "Open");
    github.add_css_class("pill");
    github_row.add_suffix(&github);
    project_group.add(&github_row);

    add_info_row(
        &project_group,
        "Credits",
        Some("Core projects and technologies"),
        "Smithay, GTK4, PipeWire, Rust",
    );
    page.add(&project_group);

    adw::NavigationPage::new(&page, "About")
}

fn displays_page(
    config: Rc<RefCell<FocalDeskConfig>>,
    window: adw::ApplicationWindow,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Displays");

    let detected_displays = Rc::new(RefCell::new(load_displays()));
    let runtime_statuses = load_display_runtime_statuses();
    apply_runtime_statuses(&detected_displays, &runtime_statuses, false);
    let row_registry: Rc<RefCell<HashMap<String, adw::ExpanderRow>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let hdr_switch_registry: Rc<RefCell<HashMap<String, gtk::Switch>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let bulk_hdr_update = Rc::new(Cell::new(false));
    let hdr_requests_dirty = Rc::new(Cell::new(false));

    let all_hdr_group = adw::PreferencesGroup::new();
    all_hdr_group.set_title("Experimental HDR10");
    all_hdr_group.set_description(Some(
        "HDR10 must be applied to every enabled capable display so identical panels match. Mixed HDR10 and SDR will not look the same.",
    ));
    let all_hdr_row = adw::ActionRow::new();
    all_hdr_row.set_title("Requested HDR10 outputs");
    let all_hdr_button = gtk::Button::with_label("Apply Requested HDR10");
    all_hdr_button.set_hexpand(true);
    all_hdr_button.set_halign(gtk::Align::Fill);
    all_hdr_button.set_height_request(48);
    all_hdr_button.add_css_class("destructive-action");
    refresh_all_outputs_hdr_control(&detected_displays.borrow(), &all_hdr_row, &all_hdr_button);
    all_hdr_group.add(&all_hdr_row);
    all_hdr_group.add(&all_hdr_button);
    let arrangement_group = adw::PreferencesGroup::new();
    arrangement_group.set_title("Arrangement");
    arrangement_group.set_description(Some(
        "Drag displays to arrange their logical desktop positions",
    ));

    let area = monitor_arrangement_area(detected_displays.clone());
    arrangement_group.add(&area);

    page.add(&arrangement_group);

    let outputs_group = adw::PreferencesGroup::new();
    outputs_group.set_title("Connected Displays");

    let display_count = detected_displays.borrow().len();
    if display_count == 0 {
        let row = adw::ActionRow::new();
        row.set_title("No connected displays found");
        row.set_subtitle("Display information will appear here after the compositor writes it.");
        outputs_group.add(&row);
    } else {
        for index in 0..display_count {
            let row = connected_display_row(
                index,
                detected_displays.clone(),
                area.clone(),
                row_registry.clone(),
                hdr_switch_registry.clone(),
                bulk_hdr_update.clone(),
                hdr_requests_dirty.clone(),
                all_hdr_row.clone(),
                all_hdr_button.clone(),
                window.clone(),
            );
            outputs_group.add(&row);
        }
    }
    page.add(&outputs_group);
    page.add(&all_hdr_group);

    {
        let displays = detected_displays.clone();
        let area = area.clone();
        let row_registry = row_registry.clone();
        let hdr_switch_registry = hdr_switch_registry.clone();
        let bulk_hdr_update = bulk_hdr_update.clone();
        let hdr_requests_dirty = hdr_requests_dirty.clone();
        let all_hdr_row = all_hdr_row.clone();
        let all_hdr_button_for_handler = all_hdr_button.clone();
        all_hdr_button.connect_clicked(move |_| {
            {
                let mut displays = displays.borrow_mut();
                align_hdr_requests_across_capable_outputs(&mut displays);
            }
            bulk_hdr_update.set(true);
            for display in displays.borrow().iter() {
                if let Some(row) = row_registry.borrow().get(&display.name) {
                    row.set_subtitle(&display_summary(display));
                }
                if let Some(switch) = hdr_switch_registry.borrow().get(&display.name) {
                    set_switch_if_changed(switch, display.hdr_requested || display.hdr_enabled);
                }
            }
            bulk_hdr_update.set(false);

            persist_displays(&displays.borrow());
            match apply_displays_to_desktop(&displays.borrow()) {
                Ok(()) => {
                    hdr_requests_dirty.set(false);
                    all_hdr_row.set_subtitle(
                        "HDR10 request sent to focaldesk-desktop; waiting for KMS validation…",
                    );
                }
                Err(err) => {
                    all_hdr_row.set_subtitle(&format!(
                        "Could not apply HDR10 through focaldesk-desktop: {err}"
                    ));
                }
            }
            area.queue_draw();
            all_hdr_button_for_handler.set_sensitive(true);
        });
    }

    // Layout group
    let layout_group = adw::PreferencesGroup::new();
    layout_group.set_title("Layout");

    let topbar_row = adw::ActionRow::new();
    topbar_row.set_title("Show top bar on all displays");
    topbar_row.set_subtitle("When disabled, only the focused display shows the top bar");

    let topbar_switch = gtk::Switch::new();
    topbar_switch.set_active(config.borrow().displays.topbar_on_all_outputs);

    topbar_row.add_suffix(&topbar_switch);
    topbar_row.set_activatable_widget(Some(&topbar_switch));

    {
        let config = config.clone();
        topbar_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.topbar_on_all_outputs = active;
            persist_config_key(
                &config.borrow(),
                "displays.topbar_on_all_outputs",
                json!(active),
            );
        });
    }

    layout_group.add(&topbar_row);

    let sidebar_row = adw::ActionRow::new();
    sidebar_row.set_title("Show sidebar on all displays");
    sidebar_row.set_subtitle("When disabled, only the focused display shows the sidebar");

    let sidebar_switch = gtk::Switch::new();
    sidebar_switch.set_active(config.borrow().displays.sidebar_on_all_outputs);

    sidebar_row.add_suffix(&sidebar_switch);
    sidebar_row.set_activatable_widget(Some(&sidebar_switch));

    {
        let config = config.clone();
        sidebar_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.sidebar_on_all_outputs = active;
            persist_config_key(
                &config.borrow(),
                "displays.sidebar_on_all_outputs",
                json!(active),
            );
        });
    }

    layout_group.add(&sidebar_row);

    // Focus group
    let focus_group = adw::PreferencesGroup::new();
    focus_group.set_title("Focus");

    let remember_row = adw::ActionRow::new();
    remember_row.set_title("Remember focused display");
    remember_row.set_subtitle("Restore the last active display when FocalDesk starts");

    let remember_switch = gtk::Switch::new();
    remember_switch.set_active(config.borrow().displays.remember_focused_output);

    remember_row.add_suffix(&remember_switch);
    remember_row.set_activatable_widget(Some(&remember_switch));

    {
        let config = config.clone();
        remember_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.remember_focused_output = active;
            persist_config_key(
                &config.borrow(),
                "displays.remember_focused_output",
                json!(active),
            );
        });
    }

    focus_group.add(&remember_row);

    page.add(&layout_group);
    page.add(&focus_group);

    {
        let rx = start_config_watch(&[
            "displays.topbar_on_all_outputs",
            "displays.sidebar_on_all_outputs",
            "displays.remember_focused_output",
        ]);
        let config = config.clone();
        let topbar_switch = topbar_switch.clone();
        let sidebar_switch = sidebar_switch.clone();
        let remember_switch = remember_switch.clone();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event.key.as_str() {
                    "displays.topbar_on_all_outputs" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.topbar_on_all_outputs = active;
                            set_switch_if_changed(&topbar_switch, active);
                        }
                    }
                    "displays.sidebar_on_all_outputs" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.sidebar_on_all_outputs = active;
                            set_switch_if_changed(&sidebar_switch, active);
                        }
                    }
                    "displays.remember_focused_output" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.remember_focused_output = active;
                            set_switch_if_changed(&remember_switch, active);
                        }
                    }
                    _ => {}
                }
            }

            glib::ControlFlow::Continue
        });
    }

    {
        for display in detected_displays.borrow().iter() {
            if let Some(row) = row_registry.borrow().get(&display.name) {
                row.set_subtitle(&display_summary(display));
            }
        }
        refresh_all_outputs_hdr_control(&detected_displays.borrow(), &all_hdr_row, &all_hdr_button);

        let displays = detected_displays.clone();
        let row_registry = row_registry.clone();
        let hdr_switch_registry = hdr_switch_registry.clone();
        let bulk_hdr_update = bulk_hdr_update.clone();
        let hdr_requests_dirty = hdr_requests_dirty.clone();
        let all_hdr_row = all_hdr_row.clone();
        let all_hdr_button = all_hdr_button.clone();
        let rx = start_config_watch(&["displays.runtime"]);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                if event.key.as_str() != "displays.runtime" {
                    continue;
                }

                let statuses =
                    serde_json::from_value::<Vec<DisplayRuntimeOutputStatus>>(event.value)
                        .unwrap_or_default();
                apply_runtime_statuses(&displays, &statuses, hdr_requests_dirty.get());
                bulk_hdr_update.set(true);
                for display in displays.borrow().iter() {
                    if let Some(row) = row_registry.borrow().get(&display.name) {
                        row.set_subtitle(&display_summary(display));
                    }
                    if let Some(switch) = hdr_switch_registry.borrow().get(&display.name) {
                        set_switch_if_changed(switch, display.hdr_requested || display.hdr_enabled);
                    }
                }
                bulk_hdr_update.set(false);
                refresh_all_outputs_hdr_control(&displays.borrow(), &all_hdr_row, &all_hdr_button);
            }

            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Displays")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgb_close(left: [f64; 3], right: [f64; 3]) {
        for (left, right) in left.into_iter().zip(right) {
            assert!((left - right).abs() < 1.0e-9, "{left} != {right}");
        }
    }

    #[test]
    fn theme_editor_rgb_and_hsv_controls_round_trip() {
        for rgb in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.15, 0.63, 0.92],
            [0.42, 0.42, 0.42],
        ] {
            let (hue, saturation, value) = rgb_to_hsv(rgb);
            assert_rgb_close(hsv_to_rgb(hue, saturation, value), rgb);
        }
    }

    #[test]
    fn theme_editor_hue_ring_maps_cardinal_points_and_wraps() {
        assert_eq!(hue_from_ring_point(200.0, 200.0, 182.0, 100.0), 0.0);
        assert_eq!(hue_from_ring_point(200.0, 200.0, 100.0, 182.0), 90.0);
        assert_eq!(hue_from_ring_point(200.0, 200.0, 18.0, 100.0), 180.0);
        assert_eq!(hue_from_ring_point(200.0, 200.0, 100.0, 18.0), 270.0);
        assert_eq!(wrap_hue(-1.0), 359.0);
        assert_eq!(wrap_hue(360.0), 0.0);
        assert_eq!(wrap_hue(361.0), 1.0);
    }

    #[test]
    fn theme_editor_hue_ring_hit_testing_and_drag_coordinates() {
        assert!(point_is_on_hue_ring(200.0, 200.0, 182.0, 100.0));
        assert!(!point_is_on_hue_ring(200.0, 200.0, 100.0, 100.0));
        // A drag beginning at the right edge and moving to the bottom is 90°.
        let origin = (182.0, 100.0);
        let offset = (-82.0, 82.0);
        assert_eq!(
            hue_from_ring_point(200.0, 200.0, origin.0 + offset.0, origin.1 + offset.1),
            90.0
        );
    }

    #[test]
    fn theme_editor_square_maps_and_clamps_pointer_coordinates() {
        // A 300x200 widget contains a centered 184x184 picker square.
        let (saturation, value) = saturation_value_from_point(300.0, 200.0, 58.0, 8.0);
        assert_eq!((saturation, value), (0.0, 1.0));
        let (saturation, value) = saturation_value_from_point(300.0, 200.0, 242.0, 192.0);
        assert_eq!((saturation, value), (1.0, 0.0));
        let (saturation, value) = saturation_value_from_point(300.0, 200.0, 150.0, 100.0);
        assert!((saturation - 0.5).abs() < f64::EPSILON);
        assert!((value - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn theme_editor_first_p3_switch_preserves_color_appearance() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.88, 0.74, 0.8);
        let original = draft.current_color();
        draft.switch_space(ThemeColorSpace::DisplayP3);
        let round_trip = draft.current_color().converted_to(ThemeColorSpace::Srgb);
        for (left, right) in original
            .components()
            .into_iter()
            .zip(round_trip.components())
        {
            assert!((left - right).abs() < 0.000_1, "{left} != {right}");
        }
    }

    #[test]
    fn theme_editor_preserves_out_of_srgb_p3_draft_across_switches() {
        let mut draft = ThemeEditorDraft::new(0.0, 1.0, 1.0, 1.0);
        draft.switch_space(ThemeColorSpace::DisplayP3);
        draft.hue = 0.0;
        draft.saturation = 1.0;
        draft.value = 1.0;
        let p3_red = draft.current_color();
        assert!(!p3_red.is_in_srgb_gamut());

        draft.switch_space(ThemeColorSpace::Srgb);
        assert_eq!(draft.space(), ThemeColorSpace::Srgb);
        draft.switch_space(ThemeColorSpace::DisplayP3);
        assert_eq!(draft.space(), ThemeColorSpace::DisplayP3);
        assert_eq!(draft.current_color(), p3_red);
    }

    #[test]
    fn theme_editor_classifies_p3_picker_points_against_srgb() {
        assert!(!picker_point_is_in_srgb(
            ThemeColorSpace::DisplayP3,
            0.0,
            1.0,
            1.0
        ));
        assert!(picker_point_is_in_srgb(
            ThemeColorSpace::DisplayP3,
            0.0,
            0.0,
            0.5
        ));
        assert!(picker_point_is_in_srgb(
            ThemeColorSpace::Srgb,
            120.0,
            1.0,
            1.0
        ));
    }

    #[test]
    fn theme_editor_p3_boundary_tracks_hue_and_stays_normalized() {
        let red_boundary = srgb_gamut_boundary_segments(0.0, 32);
        let green_boundary = srgb_gamut_boundary_segments(120.0, 32);
        assert!(!red_boundary.is_empty());
        assert!(!green_boundary.is_empty());
        assert_ne!(red_boundary, green_boundary);
        assert!(red_boundary
            .iter()
            .flatten()
            .all(|coordinate| (0.0..=1.0).contains(coordinate)));
        assert!(srgb_gamut_boundary_segments(0.0, 1).is_empty());
    }

    #[test]
    fn theme_editor_gradient_stop_lifecycle_keeps_two_minimum() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.switch_mode(1);
        draft.add_stop();
        assert_eq!(draft.stops.len(), 3);
        assert_eq!(draft.stops[draft.selected_stop].position, 0.5);
        draft.duplicate_stop();
        assert_eq!(draft.stops.len(), 4);
        assert_eq!(draft.stops[draft.selected_stop].position, 0.55);
        draft.remove_stop();
        draft.remove_stop();
        draft.remove_stop();
        assert_eq!(draft.stops.len(), 2);
    }

    #[test]
    fn theme_editor_gradient_paint_sorts_stops_and_keeps_geometry() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.switch_mode(1);
        draft.stops[0].position = 0.9;
        draft.stops[1].position = 0.1;
        draft.linear_angle = 42.0;
        let ThemePaint::LinearGradient { angle, stops, .. } = draft.paint() else {
            panic!("expected linear gradient");
        };
        assert_eq!(angle, 42.0);
        assert_eq!(stops[0].position, 0.1);
        assert_eq!(stops[1].position, 0.9);

        draft.switch_mode(2);
        draft.radial_center = (0.25, 0.75);
        draft.radial_radius = 1.2;
        let ThemePaint::RadialGradient { center, radius, .. } = draft.paint() else {
            panic!("expected radial gradient");
        };
        assert_eq!(center, (0.25, 0.75));
        assert_eq!(radius, 1.2);
    }

    #[test]
    fn theme_editor_gradient_stop_owns_color_gamut_and_alpha() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.switch_mode(1);
        draft.switch_space(ThemeColorSpace::DisplayP3);
        draft.hue = 0.0;
        draft.saturation = 1.0;
        draft.value = 1.0;
        draft.alpha = 0.4;
        draft.select_stop(1);

        let first = draft.stops[0].color.color();
        assert_eq!(first.space, ThemeColorSpace::DisplayP3);
        assert!((first.a - 0.4).abs() < f32::EPSILON);
        assert!(!first.is_in_srgb_gamut());
        assert_eq!(draft.stops[1].color.color().space, ThemeColorSpace::Srgb);
    }

    #[test]
    fn theme_editor_gradient_stop_hit_testing_selects_nearest() {
        let color = ThemeEditorColor::new(ThemeColor::srgb(0.0, 0.0, 0.0, 1.0));
        let stops = vec![
            ThemeEditorStop {
                position: 0.1,
                color: color.clone(),
            },
            ThemeEditorStop {
                position: 0.8,
                color,
            },
        ];
        assert_eq!(nearest_gradient_stop(&stops, 0.2), Some(0));
        assert_eq!(nearest_gradient_stop(&stops, 0.7), Some(1));
        assert_eq!(nearest_gradient_stop(&[], 0.5), None);
    }

    #[test]
    fn theme_editor_gradient_has_toml_serialization_shape() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.switch_mode(1);
        let encoded = toml::to_string(&draft.paint()).unwrap();
        assert!(encoded.contains("mode = \"linear_gradient\""));
        assert_eq!(encoded.matches("position =").count(), 2);
        assert!(encoded.contains("space = \"srgb\""));
    }

    #[test]
    fn theme_editor_dynamic_range_does_not_change_paint_or_luminance_setting() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.hdr_luminance_nits = 650.0;
        let source = draft.paint();

        draft.dynamic_range = ThemeDynamicRange::Hdr;
        assert_eq!(draft.paint(), source);
        assert_eq!(draft.hdr_luminance_nits, 650.0);
        draft.dynamic_range = ThemeDynamicRange::Sdr;
        assert_eq!(draft.paint(), source);
        assert_eq!(draft.hdr_luminance_nits, 650.0);
    }

    #[test]
    fn theme_editor_paint_intent_wraps_solid_and_gradient_paints() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.9, 1.0);
        draft.dynamic_range = ThemeDynamicRange::Hdr;
        draft.hdr_luminance_nits = 800.0;
        let solid = draft.paint_intent();
        assert_eq!(solid.dynamic_range, ThemeDynamicRange::Hdr);
        assert_eq!(solid.hdr_luminance_nits, 800.0);
        assert!(matches!(solid.paint, ThemePaint::Solid { .. }));

        draft.switch_mode(1);
        let gradient = draft.paint_intent();
        assert_eq!(gradient.dynamic_range, ThemeDynamicRange::Hdr);
        assert_eq!(gradient.hdr_luminance_nits, 800.0);
        assert!(matches!(gradient.paint, ThemePaint::LinearGradient { .. }));
    }

    #[test]
    fn theme_editor_hdr_preview_does_not_mutate_source_paint() {
        let mut draft = ThemeEditorDraft::new(205.0, 0.8, 0.5, 1.0);
        draft.dynamic_range = ThemeDynamicRange::Hdr;
        draft.hdr_luminance_nits = 1_000.0;
        let source = draft.paint();
        let preview = draft.preview_paint();

        assert_ne!(preview, source);
        assert_eq!(draft.paint(), source);
    }

    #[test]
    fn theme_editor_document_restores_gradient_hdr_and_gamut_metadata() {
        let mut document = ThemeDocument::new(
            "Polar Light",
            ThemePaintIntent {
                paint: ThemePaint::LinearGradient {
                    angle: 73.0,
                    interpolation: GradientInterpolation {
                        space: ThemeColorSpace::DisplayP3,
                        premultiplied_alpha: true,
                    },
                    stops: vec![
                        GradientStop {
                            position: 0.0,
                            color: ThemeColor::display_p3(1.0, 0.2, 0.1, 0.7),
                        },
                        GradientStop {
                            position: 1.0,
                            color: ThemeColor::srgb(0.1, 0.3, 0.8, 1.0),
                        },
                    ],
                },
                dynamic_range: ThemeDynamicRange::Hdr,
                hdr_luminance_nits: 750.0,
            },
        );
        document.wallpaper = ThemeWallpaper {
            path: Some("/tmp/polar-light.png".to_string()),
            fit: ThemeWallpaperFit::Tile,
            tint: Some(ThemeColor::srgb(0.1, 0.2, 0.3, 0.4)),
            dim: 0.25,
        };
        let draft = ThemeEditorDraft::from_document(&document).unwrap();
        assert_eq!(draft.theme_name, "Polar Light");
        assert_eq!(draft.mode, 1);
        assert_eq!(draft.linear_angle, 73.0);
        assert_eq!(draft.interpolation_space, ThemeColorSpace::DisplayP3);
        assert_eq!(draft.dynamic_range, ThemeDynamicRange::Hdr);
        assert_eq!(draft.hdr_luminance_nits, 750.0);
        assert_eq!(
            draft.stops[0].color.color().space,
            ThemeColorSpace::DisplayP3
        );
        assert_eq!(draft.stops[1].color.color().space, ThemeColorSpace::Srgb);
        assert_eq!(draft.wallpaper, document.wallpaper);
    }

    #[test]
    fn theme_editor_rejects_documents_it_cannot_edit_losslessly() {
        let document = ThemeDocument::new(
            "Future gamut",
            ThemePaintIntent::new(ThemePaint::solid(ThemeColor::rec2020(0.5, 0.5, 0.5, 1.0))),
        );
        assert!(ThemeEditorDraft::from_document(&document)
            .unwrap_err()
            .contains("Rec.2020"));
    }

    #[test]
    fn theme_editor_save_path_adds_toml_extension_only_when_missing() {
        assert_eq!(
            theme_editor_toml_path(PathBuf::from("aurora")),
            PathBuf::from("aurora.toml")
        );
        assert_eq!(
            theme_editor_toml_path(PathBuf::from("aurora.theme")),
            PathBuf::from("aurora.theme")
        );
        assert_eq!(
            theme_package_path(PathBuf::from("aurora")),
            PathBuf::from("aurora.fdtheme")
        );
    }

    #[test]
    fn theme_editor_runtime_status_reports_preview_and_gradient_capability() {
        let status = ThemeEditorRuntimeStatus {
            preview_active: true,
            applied_revision: 4,
            gradient_rendering: true,
            semantic_rendering: true,
            wallpaper_processing: true,
            layout_metrics: true,
            typography_metrics: true,
            contrast_issue_count: 0,
        };
        assert_eq!(
            theme_editor_runtime_label(status, false),
            "Connected · preview active · 0 contrast issues"
        );
        assert_eq!(
            theme_editor_runtime_label(
                ThemeEditorRuntimeStatus {
                    gradient_rendering: false,
                    ..status
                },
                true
            ),
            "Connected · gradients preview at their midpoint"
        );
    }

    #[test]
    fn semantic_preview_contrast_flags_legibility_difference() {
        let black = ThemeColor::srgb(0.0, 0.0, 0.0, 1.0);
        let white = ThemeColor::srgb(1.0, 1.0, 1.0, 1.0);
        assert!(semantic_contrast_ratio(black, white) > 20.0);
        assert!(semantic_contrast_ratio(white, white) < 1.1);
    }

    #[test]
    fn hdr_status_distinguishes_request_from_active_output() {
        assert_eq!(hdr_status_subtitle(true, true), "Active now");
        assert_eq!(hdr_status_subtitle(true, false), "Requested, but inactive");
        assert_eq!(hdr_status_subtitle(false, false), "Off");
    }

    #[test]
    fn chrome_order_parser_accepts_empty_and_numeric_lists() {
        assert_eq!(parse_chrome_order(""), Some(vec![]));
        assert_eq!(
            parse_chrome_order("104, 100,101"),
            Some(vec![104, 100, 101])
        );
        assert_eq!(parse_chrome_order("network, 100"), None);
    }

    #[test]
    fn display_output_config_keeps_hdr_request_and_runtime_state_separate() {
        let display = DisplayConfig {
            name: "DP-1".to_string(),
            enabled: true,
            mode_width: 3840,
            mode_height: 2160,
            refresh_mhz: 60_000,
            available_modes: Vec::new(),
            scale: 1.0,
            logical_x: 0,
            logical_y: 0,
            physical_width_mm: None,
            physical_height_mm: None,
            primary: true,
            transform: "Normal".to_string(),
            color_profile: DisplayColorProfile::Auto,
            icc_profile_path: None,
            hdr_supported: true,
            hdr_requested: true,
            hdr_enabled: false,
            hdr_appearance: HdrAppearance::default(),
            icc_lut_fallback_active: false,
            wide_gamut_active: false,
            exclusive_hdr_phase: ExclusiveHdrPhase::Off,
            exclusive_hdr_reason: None,
        };

        let output = output_config_from_display(&display);

        assert!(output.hdr_requested);
        assert!(!output.hdr_enabled);
        assert_eq!(output.hdr_appearance, HdrAppearance::default());
    }

    fn test_display(name: &str, hdr_requested: bool) -> DisplayConfig {
        DisplayConfig {
            name: name.to_string(),
            enabled: true,
            mode_width: 2560,
            mode_height: 1440,
            refresh_mhz: 120_000,
            available_modes: Vec::new(),
            scale: 1.0,
            logical_x: 0,
            logical_y: 0,
            physical_width_mm: None,
            physical_height_mm: None,
            primary: name == "DP-3",
            transform: "Normal".to_string(),
            color_profile: DisplayColorProfile::Auto,
            icc_profile_path: None,
            hdr_supported: true,
            hdr_requested,
            hdr_enabled: false,
            hdr_appearance: HdrAppearance::default(),
            icc_lut_fallback_active: false,
            wide_gamut_active: false,
            exclusive_hdr_phase: ExclusiveHdrPhase::Off,
            exclusive_hdr_reason: None,
        }
    }

    #[test]
    fn refresh_selector_uses_modes_for_current_resolution() {
        let mut display = test_display("DP-4", true);
        display.available_modes = vec![
            DisplayModeConfig {
                width: 2560,
                height: 1440,
                refresh_mhz: 120_000,
            },
            DisplayModeConfig {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            },
            DisplayModeConfig {
                width: 2560,
                height: 1440,
                refresh_mhz: 60_000,
            },
        ];
        assert_eq!(refresh_options(&display, 2560, 1440), vec![60_000, 120_000]);
    }

    #[test]
    fn apply_hdr10_requests_every_capable_sibling() {
        let mut displays = vec![test_display("DP-3", false), test_display("DP-4", true)];
        assert!(align_hdr_requests_across_capable_outputs(&mut displays));
        assert!(displays.iter().all(|display| display.hdr_requested));

        let mut already_aligned = vec![test_display("DP-3", true), test_display("DP-4", true)];
        assert!(!align_hdr_requests_across_capable_outputs(
            &mut already_aligned
        ));

        let mut none_requested = vec![test_display("DP-3", false), test_display("DP-4", false)];
        assert!(!align_hdr_requests_across_capable_outputs(
            &mut none_requested
        ));
        assert!(none_requested.iter().all(|display| !display.hdr_requested));
    }

    #[test]
    fn existing_display_files_receive_neutral_hdr_appearance_defaults() {
        let mut value = serde_json::to_value(test_display("DP-3", true)).unwrap();
        value.as_object_mut().unwrap().remove("hdr_appearance");
        let restored: DisplayConfig = serde_json::from_value(value).unwrap();
        assert_eq!(restored.hdr_appearance, HdrAppearance::default());
    }

    #[test]
    fn hdr_appearance_presets_are_valid_and_custom_values_are_detected() {
        for selected in 0..HDR_APPEARANCE_CUSTOM_PRESET {
            let appearance = hdr_appearance_preset(selected).unwrap();
            assert_eq!(appearance.validate(), Ok(appearance));
            assert_eq!(hdr_appearance_preset_index(appearance), selected);
        }

        let custom = HdrAppearance {
            reference_white_nits: 225.0,
            ..HdrAppearance::default()
        };
        assert_eq!(
            hdr_appearance_preset_index(custom),
            HDR_APPEARANCE_CUSTOM_PRESET
        );
        assert!(hdr_appearance_preset(HDR_APPEARANCE_CUSTOM_PRESET).is_none());
    }
}
