use adw::prelude::*;
use focaldesk_config::{load_config, save_config, FocalDeskConfig};
use focaldesk_ipc::{
    send_desktop_config, send_desktop_request, send_desktop_set, watch_desktop_keys,
    DisplayRuntimeOutputStatus, IpcRequest, IpcResponse,
};
use focaldesk_logging::{init_default_logging, session_id};
use focaldesk_power::{PowerCommand, PowerManager};
use focaldesk_settings_core::{
    load_settings, save_settings, BrowserLaunchBackend, DebugLogLevel, LidCloseAction,
    LowBatteryAction, OutputConfig, PerformanceMode, PowerButtonAction, Settings,
    DisplayColorProfile,
};
use focaldesk_sounds::{generate_ui_sound, SoundBuffer, UiSound, UiSoundPlayer, SAMPLE_RATE};

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

const THEME_OPTIONS: &[&str] = &["Eagle", "Moonbase", "Classic"];
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
struct BluetoothDevice {
    address: String,
    name: String,
    paired: bool,
    connected: bool,
}

#[derive(Debug, Clone)]
struct BluetoothSnapshot {
    powered: bool,
    scanning: bool,
    devices: Vec<BluetoothDevice>,
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
struct DisplayConfig {
    name: String,
    enabled: bool,

    mode_width: i32,
    mode_height: i32,
    refresh_mhz: i32,

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
    color_profile: DisplayColorProfile,
    #[serde(default)]
    icc_profile_path: Option<String>,
    #[serde(skip)]
    icc_lut_fallback_active: bool,
    #[serde(skip)]
    wide_gamut_active: bool,
}

fn save_displays(displays: &[DisplayConfig]) {
    let path = displays_path();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(text) = serde_json::to_string_pretty(displays) {
        let _ = std::fs::write(path, text);
    }

    let outputs = displays.iter().map(output_config_from_display).collect();

    match send_desktop_request(&IpcRequest::SetDisplays { outputs }) {
        Ok(IpcResponse::Ok) => {}
        Ok(IpcResponse::Error { message }) => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                message = %message,
                "display IPC update rejected"
            );
        }
        Ok(other) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                response = ?other,
                "unexpected display IPC response"
            );
        }
        Err(err) => {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                error = %err,
                "display IPC unavailable; saved display config directly"
            );
        }
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
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
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

fn resolution_options(current_width: i32, current_height: i32) -> Vec<(i32, i32)> {
    let mut options = vec![
        (1280, 720),
        (1366, 768),
        (1600, 900),
        (1920, 1080),
        (2560, 1440),
        (3440, 1440),
        (3840, 2160),
    ];

    if !options.contains(&(current_width, current_height)) {
        options.push((current_width, current_height));
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

fn push_unique_audio_label(devices: &mut Vec<String>, label: String, prefer_first: bool) {
    let label = normalize_audio_label(&label);
    if label.is_empty() {
        return;
    }

    let key = audio_label_key(&label);
    if devices.iter().any(|known| audio_label_key(known) == key) {
        return;
    }

    if prefer_first {
        devices.insert(0, label);
    } else {
        devices.push(label);
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
    devices: &mut Vec<String>,
    kind: AudioDeviceKind,
    default_name: Option<&str>,
) {
    if let Some(name) = current.name.as_deref() {
        if kind == AudioDeviceKind::Source && name.ends_with(".monitor") {
            return;
        }
    }

    if let Some(label) = pactl_device_label(current, ports) {
        let prefer_first = current.name.as_deref() == default_name;
        push_unique_audio_label(devices, label, prefer_first);
    }
}

fn parse_pactl_devices(
    output: &str,
    kind: AudioDeviceKind,
    default_name: Option<&str>,
) -> Vec<String> {
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
) -> Vec<String> {
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

        push_unique_audio_label(&mut devices, name.to_string(), Some(name) == default_name);
    }

    devices
}

fn parse_wpctl_devices(output: &str, kind: AudioDeviceKind) -> Vec<String> {
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

        let Some((_, label)) = trimmed.split_once(". ") else {
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
        push_unique_audio_label(&mut devices, label.to_string(), prefer_first);
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

fn load_audio_devices(kind: AudioDeviceKind) -> Result<Vec<String>, String> {
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

fn parse_bluetooth_devices(output: &str, paired: bool) -> Vec<BluetoothDevice> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            if parts.next()? != "Device" {
                return None;
            }

            let address = parts.next()?.to_string();
            let name = parts.next().unwrap_or("Unknown Device").to_string();

            Some(BluetoothDevice {
                address,
                name,
                paired,
                connected: false,
            })
        })
        .collect()
}

fn bluetooth_info_value(info: &str, key: &str) -> bool {
    info.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix(key)
            .and_then(|value| value.trim().strip_prefix(':'))
            .map(|value| value.trim() == "yes")
            .unwrap_or(false)
    })
}

fn load_bluetooth_snapshot(scanning: bool) -> BluetoothSnapshot {
    let show = match run_control_command("bluetoothctl", &["show"]) {
        Ok(output) => output,
        Err(err) => {
            return BluetoothSnapshot {
                powered: false,
                scanning,
                devices: vec![],
                error: Some(err),
            };
        }
    };

    let powered = bluetooth_info_value(&show, "Powered");
    let paired_output =
        run_control_command("bluetoothctl", &["paired-devices"]).unwrap_or_default();
    let all_output = run_control_command("bluetoothctl", &["devices"]).unwrap_or_default();

    let mut devices = parse_bluetooth_devices(&paired_output, true);

    for device in parse_bluetooth_devices(&all_output, false) {
        if !devices.iter().any(|known| known.address == device.address) {
            devices.push(device);
        }
    }

    for device in &mut devices {
        if let Ok(info) = run_control_command("bluetoothctl", &["info", &device.address]) {
            device.connected = bluetooth_info_value(&info, "Connected");
            device.paired = device.paired || bluetooth_info_value(&info, "Paired");
        }
    }

    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.cmp(&b.name))
    });

    BluetoothSnapshot {
        powered,
        scanning,
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

fn connected_display_row(
    index: usize,
    displays: Rc<RefCell<Vec<DisplayConfig>>>,
    area: gtk::DrawingArea,
    row_registry: Rc<RefCell<HashMap<String, adw::ExpanderRow>>>,
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
    let resolutions = resolution_options(display.mode_width, display.mode_height);
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
        resolution_dropdown.connect_selected_notify(move |dropdown| {
            let Some((width, height)) = resolutions.get(dropdown.selected() as usize).copied()
            else {
                return;
            };

            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.mode_width = width;
                display.mode_height = height;
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
        hdr_row.add_suffix(&hdr);
        hdr_row.set_activatable_widget(Some(&hdr));
        row.add_row(&hdr_row);

        {
            let displays = displays.clone();
            let area = area.clone();
            let row = row.clone();
            let hdr_row = hdr_row.clone();
            hdr.connect_active_notify(move |switch| {
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
                save_display_change(&displays, &area, &row, index);
            });
        }
    }

    let color_row = adw::ActionRow::new();
    color_row.set_title("Color profile");
    color_row.set_subtitle("Choose the output profile the compositor should advertise");
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
            dialog.open(
                Some(&parent),
                None::<&gtk::gio::Cancellable>,
                {
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
                                        .map(|p| {
                                            format!(
                                                "Selected: {}",
                                                display_icc_profile_label(p)
                                            )
                                        })
                                        .unwrap_or_else(|| "No ICC file selected".to_string());
                                    icc_row.set_subtitle(&subtitle);
                                }
                                save_display_change(&displays, &area, &row, index);
                            }
                        }
                    }
                },
            );
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

    if let Err(err) = send_desktop_request(&IpcRequest::Reload) {
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

        if display.icc_lut_fallback_active != fallback_active {
            display.icc_lut_fallback_active = fallback_active;
        }
        if display.wide_gamut_active != wide_gamut_active {
            display.wide_gamut_active = wide_gamut_active;
        }
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

fn main() {
    init_default_logging();
    let app = adw::Application::new(
        Some("com.focaldesk.Settings"),
        gtk::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &adw::Application) {
    let config = Rc::new(RefCell::new(load_config()));
    let settings = Rc::new(RefCell::new(load_settings()));

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("FocalDesk Settings"));
    window.set_default_size(1000, 700);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let split = adw::NavigationSplitView::new();

    // ----- sidebar -----
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);

    for name in [
        "Appearance",
        "Network",
        "Bluetooth",
        "Printers",
        "Displays",
        "Sound",
        "Applications",
        "Workspaces",
        "Keyboard",
        "Privacy",
        "Power",
        "Debug",
        "About",
    ] {
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

    pages.insert("Appearance".to_string(), appearance_page(config.clone()));
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
    pages.insert("Workspaces".to_string(), workspaces_page(settings.clone()));
    pages.insert("Keyboard".to_string(), keyboard_page());
    pages.insert("Privacy".to_string(), privacy_page(settings.clone()));
    pages.insert("Power".to_string(), power_page(settings.clone()));
    pages.insert("Debug".to_string(), debug_page(settings.clone()));
    pages.insert("About".to_string(), about_page());

    for (name, page) in &pages {
        content_stack.add_named(page, Some(name.as_str()));
    }

    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    content_stack.set_visible_child_name("Appearance");
    list.select_row(list.row_at_index(0).as_ref());

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

fn appearance_page(config: Rc<RefCell<FocalDeskConfig>>) -> adw::NavigationPage {
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
            "appearance.output_focus_glow",
            "appearance.theme",
            "appearance.glow_strength",
            "appearance.font_scale",
        ]);
        let config = config.clone();
        let shader_switch = shader_switch.clone();
        let focus_switch = focus_switch.clone();
        let theme_dropdown = theme_dropdown.clone();
        let glow_scale = glow_scale.clone();
        let font_scale = font_scale.clone();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event.key.as_str() {
                    "appearance.shader_chrome" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.shader_chrome = active;
                            set_switch_if_changed(&shader_switch, active);
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
    status: &gtk::Label,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set_text(err);
    } else if snapshot.devices.is_empty() {
        status.set_text("No Ethernet devices found");
    } else {
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
    status: &gtk::Label,
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
    status: &gtk::Label,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set_text(err);
    } else if snapshot.enabled {
        status.set_text("Wi-Fi is enabled");
    } else {
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
    status: &gtk::Label,
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

    let ethernet_status = dim_label("Loading Ethernet state");

    let ethernet_group = adw::PreferencesGroup::new();
    ethernet_group.set_title("Ethernet");

    let ethernet_refresh_row = adw::ActionRow::new();
    ethernet_refresh_row.set_title("Refresh Wired Devices");
    ethernet_refresh_row.set_subtitle("Update adapter state and active wired connections");
    let ethernet_refresh_button = gtk::Button::with_label("Refresh");
    ethernet_refresh_button.add_css_class("pill");
    ethernet_refresh_row.add_suffix(&ethernet_refresh_button);
    ethernet_group.add(&ethernet_refresh_row);
    ethernet_group.add(&ethernet_status);
    page.add(&ethernet_group);

    let ethernet_devices_group = adw::PreferencesGroup::new();
    ethernet_devices_group.set_title("Wired Devices");
    page.add(&ethernet_devices_group);
    let ethernet_device_rows = Rc::new(RefCell::new(Vec::new()));

    let wifi_status = dim_label("Loading Wi-Fi state");

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
    controls_group.add(&wifi_status);
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
    status: &gtk::Label,
) {
    clear_dynamic_rows(group, rows);

    if let Some(err) = &snapshot.error {
        status.set_text(err);
    } else if snapshot.powered {
        status.set_text(if snapshot.scanning {
            "Bluetooth is scanning"
        } else {
            "Bluetooth is powered on"
        });
    } else {
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
                let status = status.clone();
                pair.connect_clicked(move |_| {
                    match run_control_command("bluetoothctl", &["pair", &address]) {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("Paired {address}"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }
            controls.append(&pair);
        }

        let connect = gtk::Button::with_label(if device.connected {
            "Disconnect"
        } else {
            "Connect"
        });
        connect.add_css_class("pill");
        {
            let address = device.address.clone();
            let command = if device.connected {
                "disconnect"
            } else {
                "connect"
            };
            let status = status.clone();
            connect.connect_clicked(move |_| {
                match run_control_command("bluetoothctl", &[command, &address]) {
                    Ok(output) if output.is_empty() => {
                        status.set_text(&format!("{command} sent to {address}"));
                    }
                    Ok(output) => status.set_text(&output),
                    Err(err) => status.set_text(&err),
                }
            });
        }
        controls.append(&connect);

        if device.paired {
            let trust = gtk::Button::with_label("Trust");
            trust.add_css_class("pill");
            {
                let address = device.address.clone();
                let status = status.clone();
                trust.connect_clicked(move |_| {
                    match run_control_command("bluetoothctl", &["trust", &address]) {
                        Ok(output) if output.is_empty() => {
                            status.set_text(&format!("Trusted {address}"));
                        }
                        Ok(output) => status.set_text(&output),
                        Err(err) => status.set_text(&err),
                    }
                });
            }
            controls.append(&trust);
        }

        row.add_suffix(&controls);
        add_dynamic_row(group, rows, &row);
    }
}

fn refresh_bluetooth_list_async(
    group: &adw::PreferencesGroup,
    rows: &DynamicRows,
    status: &gtk::Label,
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
            populate_bluetooth_list(&group, &rows, &snapshot, &status);
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
    let status = dim_label("Loading Bluetooth state");

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
    controls_group.add(&status);
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

            let state = if switch.is_active() { "on" } else { "off" };
            match run_control_command("bluetoothctl", &["power", state]) {
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

            let state = if switch.is_active() { "on" } else { "off" };
            match run_control_command("bluetoothctl", &["scan", state]) {
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
            let labels: Vec<&str> = devices.iter().map(String::as_str).collect();
            output_device_row.add_suffix(&dropdown_from_strings(&labels, 0));
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
            let labels: Vec<&str> = devices.iter().map(String::as_str).collect();
            input_device_row.add_suffix(&dropdown_from_strings(&labels, 0));
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

fn workspaces_page(settings: Rc<RefCell<Settings>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Workspaces");

    let behavior_group = adw::PreferencesGroup::new();
    behavior_group.set_title("Desktop Behavior");
    behavior_group.set_description(Some(
        "Controls for how workspaces appear, switch, and restore applications.",
    ));

    let count_row = adw::ActionRow::new();
    count_row.set_title("Number of workspaces");
    count_row.set_subtitle("Static workspace slots shown by the desktop shell");
    let count = gtk::SpinButton::with_range(1.0, 9.0, 1.0);
    count.set_value(4.0);
    count.set_numeric(true);
    count_row.add_suffix(&count);
    behavior_group.add(&count_row);

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
    add_switch_row(
        &behavior_group,
        "Wrap around when switching",
        Some("Continue from the last workspace back to the first"),
        true,
    );
    page.add(&behavior_group);

    let keybind_group = adw::PreferencesGroup::new();
    keybind_group.set_title("Keybind Hints");
    add_info_row(&keybind_group, "Switch to workspace 1", None, "Super+1");
    add_info_row(&keybind_group, "Switch to workspace 2", None, "Super+2");
    add_info_row(
        &keybind_group,
        "Move between workspaces",
        None,
        "Super+Arrow",
    );
    page.add(&keybind_group);

    adw::NavigationPage::new(&page, "Workspaces")
}

fn keyboard_page() -> adw::NavigationPage {
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
    add_info_row(&shortcuts_group, "Open launcher", None, "Super");
    add_info_row(&shortcuts_group, "Open terminal", None, "Super+Enter");
    add_info_row(&shortcuts_group, "Close focused window", None, "Super+Q");
    add_info_row(&shortcuts_group, "Screenshot shortcut", None, "Print");
    add_info_row(&shortcuts_group, "Lock screen shortcut", None, "Super+L");
    add_button_row(
        &shortcuts_group,
        "Reset shortcuts",
        Some("Restore the default keyboard shortcuts"),
        "Reset",
    );
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
        "Screen capture permission status",
        Some("PipeWire portal screen sharing"),
        "No active grant",
    );
    add_info_row(
        &permissions_group,
        "Microphone portal permission",
        Some("Per-app microphone permissions will appear here"),
        "Placeholder",
    );
    add_info_row(
        &permissions_group,
        "Camera portal permission",
        Some("Per-app camera permissions will appear here"),
        "Placeholder",
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

fn power_status_text(manager: &PowerManager) -> String {
    let snapshot = manager.snapshot();
    let battery = snapshot
        .batteries
        .first()
        .map(|battery| {
            let percent = battery
                .percentage
                .map(|value| format!("{value}%"))
                .unwrap_or_else(|| "unknown charge".to_string());
            let state = battery.state.as_deref().unwrap_or("unknown");
            format!("{percent}, {state}")
        })
        .unwrap_or_else(|| "No battery detected".to_string());
    let line_power = match snapshot.line_power_online {
        Some(true) => "plugged in",
        Some(false) => "on battery",
        None => "line power unknown",
    };
    let profile = snapshot
        .performance_profile
        .as_deref()
        .filter(|profile| !profile.is_empty())
        .unwrap_or("profile unknown");

    format!("{battery}; {line_power}; {profile}")
}

fn run_power_action(manager: &PowerManager, command: PowerCommand, status: &gtk::Label) {
    match manager.execute(command) {
        Ok(()) => status.set_text("Power action started"),
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
    let manager = PowerManager::new();

    let status_group = adw::PreferencesGroup::new();
    status_group.set_title("Status");
    let status_label = dim_label(&power_status_text(&manager));
    status_group.add(&status_label);
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
        let manager = manager.clone();
        let status_label = status_label.clone();
        performance_dropdown.connect_selected_notify(move |dropdown| {
            let mode = selected_performance_mode(dropdown.selected());
            settings.borrow_mut().power.performance_mode = mode;
            persist_settings(&settings.borrow());

            if let Err(err) = manager.set_performance_profile(performance_profile_name(mode)) {
                error!(
                    target: "focaldesk",
                    session_id = session_id(),
                    error = %err,
                    "failed to set performance profile"
                );
                status_label.set_text("Performance profile change failed");
            } else {
                status_label.set_text(&power_status_text(&manager));
            }
        });
    }
    page.add(&actions_group);

    let system_group = adw::PreferencesGroup::new();
    system_group.set_title("System");
    let suspend_button = add_button_row(&system_group, "Suspend now", None, "Suspend");
    {
        let manager = manager.clone();
        let status_label = status_label.clone();
        suspend_button.connect_clicked(move |_| {
            run_power_action(&manager, PowerCommand::Suspend, &status_label);
        });
    }
    let hibernate_button = add_button_row(&system_group, "Hibernate now", None, "Hibernate");
    {
        let manager = manager.clone();
        let status_label = status_label.clone();
        hibernate_button.connect_clicked(move |_| {
            run_power_action(&manager, PowerCommand::Hibernate, &status_label);
        });
    }
    let restart_button = add_button_row(&system_group, "Restart", None, "Restart");
    {
        let manager = manager.clone();
        let status_label = status_label.clone();
        restart_button.connect_clicked(move |_| {
            run_power_action(&manager, PowerCommand::Reboot, &status_label);
        });
    }
    let poweroff_button = add_button_row(&system_group, "Power off", None, "Power off");
    {
        let manager = manager.clone();
        let status_label = status_label.clone();
        poweroff_button.connect_clicked(move |_| {
            run_power_action(&manager, PowerCommand::PowerOff, &status_label);
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
    let row_registry: Rc<RefCell<HashMap<String, adw::ExpanderRow>>> =
        Rc::new(RefCell::new(HashMap::new()));
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
                window.clone(),
            );
            outputs_group.add(&row);
        }
    }
    page.add(&outputs_group);

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
        let runtime_statuses = load_display_runtime_statuses();
        apply_runtime_statuses(&detected_displays, &runtime_statuses);
        for display in detected_displays.borrow().iter() {
            if let Some(row) = row_registry.borrow().get(&display.name) {
                row.set_subtitle(&display_summary(display));
            }
        }

        let displays = detected_displays.clone();
        let row_registry = row_registry.clone();
        let rx = start_config_watch(&["displays.runtime"]);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                if event.key.as_str() != "displays.runtime" {
                    continue;
                }

                let statuses =
                    serde_json::from_value::<Vec<DisplayRuntimeOutputStatus>>(event.value)
                        .unwrap_or_default();
                apply_runtime_statuses(&displays, &statuses);
                for display in displays.borrow().iter() {
                    if let Some(row) = row_registry.borrow().get(&display.name) {
                        row.set_subtitle(&display_summary(display));
                    }
                }
            }

            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Displays")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_status_distinguishes_request_from_active_output() {
        assert_eq!(hdr_status_subtitle(true, true), "Active now");
        assert_eq!(hdr_status_subtitle(true, false), "Requested, but inactive");
        assert_eq!(hdr_status_subtitle(false, false), "Off");
    }

    #[test]
    fn display_output_config_keeps_hdr_request_and_runtime_state_separate() {
        let display = DisplayConfig {
            name: "DP-1".to_string(),
            enabled: true,
            mode_width: 3840,
            mode_height: 2160,
            refresh_mhz: 60_000,
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
            icc_lut_fallback_active: false,
        };

        let output = output_config_from_display(&display);

        assert!(output.hdr_requested);
        assert!(!output.hdr_enabled);
    }
}
