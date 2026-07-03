use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;
use focaldesk_ai::{AiPermissionRecord, list_ai_permission_records, revoke_ai_permission};
use focaldesk_config::{FocalDeskConfig, load_config, save_config};
use focaldesk_ipc::{IpcRequest, IpcResponse, send_desktop_request};
use focaldesk_permissions::request::PermissionTarget;
use focaldesk_permissions::{PermissionDecision, PermissionScope};
use focaldesk_power::{LOW_BATTERY_THRESHOLD_PERCENT, PowerSnapshot};
use focaldesk_settings_core::{
    LidCloseAction, LowBatteryAction, PerformanceMode, PowerButtonAction, PowerSettings,
    load_settings, save_settings,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn sidebar_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let fill = if selected {
        egui::Color32::from_rgb(18, 78, 130)
    } else {
        egui::Color32::TRANSPARENT
    };

    let text_color = if selected {
        egui::Color32::from_rgb(120, 200, 255)
    } else {
        egui::Color32::from_rgb(210, 220, 230)
    };

    ui.add_sized(
        [160.0, 34.0],
        egui::Button::new(egui::RichText::new(text).color(text_color).size(15.0))
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(8))
            .frame(true),
    )
}

#[derive(Debug, Clone)]
struct WifiNetwork {
    active: bool,
    ssid: String,
    security: String,
    signal: u8,
}

#[derive(Debug, Clone)]
struct EthernetDevice {
    device: String,
    state: String,
    connection: Option<String>,
}

#[derive(Debug, Clone)]
struct BluetoothDevice {
    address: String,
    name: String,
    paired: bool,
    connected: bool,
}

fn run_control_command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
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

fn focaldesk_config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("config.toml")
}

fn focaldesk_log_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(path) = std::env::var("FOCALDESK_LOG_FILE") {
        paths.push(PathBuf::from(path));
    }

    if let Some(state_dir) = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("state"))
        })
    {
        paths.push(state_dir.join("focaldesk").join("focaldesk.log"));
    }

    if let Some(cache_dir) = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".cache"))
        })
    {
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
        log_path,
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".to_string()),
        std::env::var("DISPLAY").unwrap_or_else(|_| "unset".to_string()),
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unset".to_string()),
    )
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

fn wifi_enabled() -> Result<bool, String> {
    let output = run_control_command("nmcli", &["-t", "-f", "WIFI", "radio"])?;
    Ok(output.lines().next().unwrap_or_default().trim() == "enabled")
}

fn load_wifi_networks() -> Result<Vec<WifiNetwork>, String> {
    let output = run_control_command(
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
    )?;

    let mut networks = Vec::new();

    for line in output.lines() {
        let fields = split_nmcli_line(line);
        let ssid = fields.get(1).map(String::as_str).unwrap_or_default().trim();
        if ssid.is_empty() {
            continue;
        }

        networks.push(WifiNetwork {
            active: fields.first().map(String::as_str).unwrap_or_default() == "*",
            ssid: ssid.to_string(),
            security: fields
                .get(2)
                .map(String::as_str)
                .unwrap_or_default()
                .trim()
                .to_string(),
            signal: fields
                .get(3)
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(0),
        });
    }

    networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    networks.dedup_by(|a, b| a.ssid == b.ssid);
    Ok(networks)
}

fn wifi_security_label(security: &str) -> &str {
    if security.trim().is_empty() || security == "--" {
        "Open network"
    } else {
        "Secured network"
    }
}

fn connect_wifi(ssid: &str, password: &str) -> Result<String, String> {
    if password.is_empty() {
        run_control_command("nmcli", &["device", "wifi", "connect", ssid])
    } else {
        run_control_command(
            "nmcli",
            &["device", "wifi", "connect", ssid, "password", password],
        )
    }
}

fn load_ethernet_devices() -> Result<Vec<EthernetDevice>, String> {
    let output = run_control_command(
        "nmcli",
        &[
            "-t",
            "-f",
            "DEVICE,TYPE,STATE,CONNECTION",
            "device",
            "status",
        ],
    )?;

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
    Ok(devices)
}

fn ethernet_connected(device: &EthernetDevice) -> bool {
    matches!(
        device.state.as_str(),
        "connected" | "connecting (getting IP configuration)"
    )
}

fn bluetooth_powered() -> Result<bool, String> {
    let output = run_control_command("bluetoothctl", &["show"])?;
    Ok(bluetooth_info_value(&output, "Powered"))
}

fn fetch_power_snapshot() -> Result<PowerSnapshot, String> {
    if running_in_desktop_process() {
        return Ok(focaldesk_power::PowerManager::new().snapshot());
    }

    match send_desktop_request(&IpcRequest::GetPowerSnapshot)? {
        IpcResponse::PowerSnapshot { snapshot } => Ok(snapshot),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("unexpected IPC response: {other:?}")),
    }
}

fn running_in_desktop_process() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| matches!(name, "focaldesk-desktop" | "focaldesk-server"))
        })
        .unwrap_or(false)
}

fn snapshot_age_label(snapshot: &PowerSnapshot) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(snapshot.captured_at_unix_ms);
    let age_ms = now_ms.saturating_sub(snapshot.captured_at_unix_ms);

    if age_ms < 1_000 {
        format!("{age_ms} ms old")
    } else {
        let seconds = age_ms as f64 / 1_000.0;
        format!("{seconds:.1} s old")
    }
}

fn snapshot_age_color(snapshot: &PowerSnapshot) -> egui::Color32 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(snapshot.captured_at_unix_ms);
    let age_ms = now_ms.saturating_sub(snapshot.captured_at_unix_ms);

    if age_ms < 3_000 {
        egui::Color32::from_rgb(120, 200, 255)
    } else if age_ms < 10_000 {
        egui::Color32::from_rgb(230, 180, 80)
    } else {
        egui::Color32::from_rgb(230, 90, 90)
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

            Some(BluetoothDevice {
                address: parts.next()?.to_string(),
                name: parts.next().unwrap_or("Unknown Device").to_string(),
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

fn load_bluetooth_devices() -> Result<Vec<BluetoothDevice>, String> {
    let paired_output = run_control_command("bluetoothctl", &["paired-devices"])?;
    let all_output = run_control_command("bluetoothctl", &["devices"])?;

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

    Ok(devices)
}

pub struct SettingsPanel {
    pub open: bool,
    was_open: bool,
    tab: SettingsPage,
    config: FocalDeskConfig,
    power: PowerSettings,
    wifi_passwords: HashMap<String, String>,
    network_status: String,
    bluetooth_status: String,
    bluetooth_scanning: bool,
    ai_permissions_status: String,
    debug_status: String,
    last_power_status_poll_at: Instant,
    last_power_snapshot: Option<PowerSnapshot>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Network,
    Bluetooth,
    Displays,
    Workspaces,
    Keyboard,
    Privacy,
    AiPermissions,
    Power,
    Debug,
    About,
}

impl Default for SettingsPanel {
    fn default() -> Self {
        let settings = load_settings();
        Self {
            tab: SettingsPage::Appearance,
            was_open: false,
            config: load_config(),
            power: settings.power,
            wifi_passwords: HashMap::new(),
            network_status: String::new(),
            bluetooth_status: String::new(),
            bluetooth_scanning: false,
            ai_permissions_status: String::new(),
            debug_status: "Diagnostics are generated locally".to_string(),
            last_power_status_poll_at: Instant::now() - Duration::from_secs(2),
            last_power_snapshot: None,
            open: false,
        }
    }
}

impl SettingsPanel {
    pub fn open_displays(&mut self) {
        self.tab = SettingsPage::Displays;
        self.open = true;
    }

    pub fn open_workspaces(&mut self) {
        self.tab = SettingsPage::Workspaces;
        self.open = true;
    }

    fn reload_from_disk(&mut self) {
        self.config = load_config();
        self.power = load_settings().power;
        self.network_status.clear();
        self.bluetooth_status.clear();
        self.ai_permissions_status.clear();
        self.debug_status = "Diagnostics are generated locally".to_string();
        self.last_power_status_poll_at = Instant::now() - Duration::from_secs(2);
        self.last_power_snapshot = None;
    }

    fn refresh_power_status_if_needed(&mut self) {
        let now = Instant::now();
        if now.saturating_duration_since(self.last_power_status_poll_at) < Duration::from_secs(2) {
            return;
        }

        self.last_power_status_poll_at = now;
        self.last_power_snapshot = fetch_power_snapshot().ok();
    }

    pub(crate) fn refresh_power_status_now(&mut self) {
        self.last_power_status_poll_at = Instant::now();
        self.last_power_snapshot = fetch_power_snapshot().ok();
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.separator();
        ui.add_space(8.0);

        if sidebar_button(ui, "Appearance", self.tab == SettingsPage::Appearance).clicked() {
            self.tab = SettingsPage::Appearance;
        }

        if sidebar_button(ui, "Network", self.tab == SettingsPage::Network).clicked() {
            self.tab = SettingsPage::Network;
        }

        if sidebar_button(ui, "Bluetooth", self.tab == SettingsPage::Bluetooth).clicked() {
            self.tab = SettingsPage::Bluetooth;
        }

        if sidebar_button(ui, "Displays", self.tab == SettingsPage::Displays).clicked() {
            self.tab = SettingsPage::Displays;
        }

        if sidebar_button(ui, "Workspaces", self.tab == SettingsPage::Workspaces).clicked() {
            self.tab = SettingsPage::Workspaces;
        }

        if sidebar_button(ui, "Keyboard", self.tab == SettingsPage::Keyboard).clicked() {
            self.tab = SettingsPage::Keyboard;
        }

        if sidebar_button(ui, "Privacy", self.tab == SettingsPage::Privacy).clicked() {
            self.tab = SettingsPage::Privacy;
        }

        if sidebar_button(
            ui,
            "AI Permissions",
            self.tab == SettingsPage::AiPermissions,
        )
        .clicked()
        {
            self.tab = SettingsPage::AiPermissions;
        }

        if sidebar_button(ui, "Power", self.tab == SettingsPage::Power).clicked() {
            self.tab = SettingsPage::Power;
        }

        if sidebar_button(ui, "Debug", self.tab == SettingsPage::Debug).clicked() {
            self.tab = SettingsPage::Debug;
        }

        if sidebar_button(ui, "About", self.tab == SettingsPage::About).clicked() {
            self.tab = SettingsPage::About;
        }
    }

    fn displays_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Displays");

        ui.checkbox(
            &mut self.config.displays.topbar_on_all_outputs,
            "Top bar on all outputs",
        );

        ui.checkbox(
            &mut self.config.displays.sidebar_on_all_outputs,
            "Sidebar on all outputs",
        );

        ui.checkbox(
            &mut self.config.displays.remember_focused_output,
            "Remember focused output",
        );
    }

    fn appearance_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appearance");

        let mut changed = false;

        changed |= ui
            .checkbox(
                &mut self.config.appearance.shader_chrome,
                "Use shader chrome",
            )
            .changed();

        changed |= ui
            .checkbox(
                &mut self.config.appearance.output_focus_glow,
                "Output focus glow",
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.config.appearance.glow_strength, 0.0..=1.0)
                    .text("Glow strength"),
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.config.appearance.font_scale, 0.75..=1.5)
                    .text("Font scale"),
            )
            .changed();

        if changed {
            let _ = save_config(&self.config);
        }
    }

    fn network_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Network");
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.heading("Ethernet");

            match load_ethernet_devices() {
                Ok(devices) if devices.is_empty() => {
                    ui.label("No Ethernet devices found");
                }
                Ok(devices) => {
                    for device in devices {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(&device.device);
                                ui.label(format!(
                                    "{} | {}",
                                    device.state,
                                    device
                                        .connection
                                        .as_deref()
                                        .unwrap_or("No active connection")
                                ));
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let command = if ethernet_connected(&device) {
                                        "disconnect"
                                    } else {
                                        "connect"
                                    };
                                    let label = if ethernet_connected(&device) {
                                        "Disconnect"
                                    } else {
                                        "Connect"
                                    };

                                    if ui.button(label).clicked() {
                                        self.network_status = run_control_command(
                                            "nmcli",
                                            &["device", command, &device.device],
                                        )
                                        .unwrap_or_else(|err| err);
                                    }
                                },
                            );
                        });
                    }
                }
                Err(err) => {
                    ui.label(err);
                }
            }
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Wi-Fi");

            let mut wifi_enabled = wifi_enabled().unwrap_or(false);
            if ui.checkbox(&mut wifi_enabled, "Wi-Fi enabled").changed() {
                let state = if wifi_enabled { "on" } else { "off" };
                self.network_status = run_control_command("nmcli", &["radio", "wifi", state])
                    .unwrap_or_else(|err| err);
            }

            ui.add_space(8.0);

            match load_wifi_networks() {
                Ok(networks) if networks.is_empty() => {
                    ui.label(if wifi_enabled {
                        "No Wi-Fi networks found"
                    } else {
                        "Turn on Wi-Fi to scan for networks"
                    });
                }
                Ok(networks) => {
                    for network in networks {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(if network.active {
                                    format!("{} (connected)", network.ssid)
                                } else {
                                    network.ssid.clone()
                                });
                                ui.label(format!(
                                    "{} | Signal {}%",
                                    wifi_security_label(&network.security),
                                    network.signal
                                ));
                            });

                            if !network.active {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Connect").clicked() {
                                            let password = self
                                                .wifi_passwords
                                                .get(&network.ssid)
                                                .cloned()
                                                .unwrap_or_default();
                                            self.network_status =
                                                connect_wifi(&network.ssid, &password)
                                                    .unwrap_or_else(|err| err);
                                        }

                                        if !network.security.trim().is_empty()
                                            && network.security != "--"
                                        {
                                            let password = self
                                                .wifi_passwords
                                                .entry(network.ssid.clone())
                                                .or_default();
                                            ui.add(
                                                egui::TextEdit::singleline(password)
                                                    .password(true)
                                                    .hint_text("Password")
                                                    .desired_width(160.0),
                                            );
                                        }
                                    },
                                );
                            }
                        });
                    }
                }
                Err(err) => {
                    ui.label(err);
                }
            }
        });

        if !self.network_status.is_empty() {
            ui.add_space(8.0);
            ui.label(&self.network_status);
        }
    }

    fn bluetooth_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Bluetooth");
        ui.add_space(8.0);

        let mut powered = bluetooth_powered().unwrap_or(false);
        if ui.checkbox(&mut powered, "Bluetooth powered").changed() {
            let state = if powered { "on" } else { "off" };
            self.bluetooth_status =
                run_control_command("bluetoothctl", &["power", state]).unwrap_or_else(|err| err);
        }

        if ui
            .checkbox(&mut self.bluetooth_scanning, "Scan for devices")
            .changed()
        {
            let state = if self.bluetooth_scanning { "on" } else { "off" };
            self.bluetooth_status =
                run_control_command("bluetoothctl", &["scan", state]).unwrap_or_else(|err| err);
        }

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Devices");

            match load_bluetooth_devices() {
                Ok(devices) if devices.is_empty() => {
                    ui.label(if powered {
                        "No Bluetooth devices found"
                    } else {
                        "Turn on Bluetooth to manage devices"
                    });
                }
                Ok(devices) => {
                    for device in devices {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(&device.name);
                                ui.label(format!(
                                    "{}{} | {}",
                                    if device.connected {
                                        "Connected"
                                    } else {
                                        "Disconnected"
                                    },
                                    if device.paired { " | Paired" } else { "" },
                                    device.address
                                ));
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let command = if device.connected {
                                        "disconnect"
                                    } else {
                                        "connect"
                                    };
                                    let label = if device.connected {
                                        "Disconnect"
                                    } else {
                                        "Connect"
                                    };

                                    if ui.button(label).clicked() {
                                        self.bluetooth_status = run_control_command(
                                            "bluetoothctl",
                                            &[command, &device.address],
                                        )
                                        .unwrap_or_else(|err| err);
                                    }

                                    if device.paired && ui.button("Trust").clicked() {
                                        self.bluetooth_status = run_control_command(
                                            "bluetoothctl",
                                            &["trust", &device.address],
                                        )
                                        .unwrap_or_else(|err| err);
                                    }

                                    if !device.paired && ui.button("Pair").clicked() {
                                        self.bluetooth_status = run_control_command(
                                            "bluetoothctl",
                                            &["pair", &device.address],
                                        )
                                        .unwrap_or_else(|err| err);
                                    }
                                },
                            );
                        });
                    }
                }
                Err(err) => {
                    ui.label(err);
                }
            }
        });

        if !self.bluetooth_status.is_empty() {
            ui.add_space(8.0);
            ui.label(&self.bluetooth_status);
        }
    }

    fn power_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        ui.heading("Power");
        ui.add_space(8.0);

        ui.label("Changes are saved immediately.");
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.heading("Status");
            ui.add_space(4.0);
            ui.label(format!(
                "Configured performance mode: {}",
                performance_mode_label(self.power.performance_mode)
            ));
            self.refresh_power_status_if_needed();
            match self.last_power_snapshot.as_ref() {
                Some(snapshot) => {
                    let active_profile = snapshot
                        .performance_profile
                        .as_deref()
                        .filter(|profile| !profile.is_empty());
                    match active_profile {
                        Some(profile) => {
                            let active_label = match profile {
                                "balanced" => "Balanced",
                                "performance" => "Performance",
                                "power-saver" | "powersaver" => "Power saver",
                                other => other,
                            };
                            ui.label(format!("Active performance profile: {active_label}"));
                            if profile != performance_mode_profile_name(self.power.performance_mode)
                            {
                                ui.label("System profile differs from the configured target.");
                            }
                        }
                        None => {
                            ui.label("Active performance profile: unknown");
                        }
                    }

                    match snapshot.line_power_online {
                        Some(true) => ui.label("AC power: connected"),
                        Some(false) => ui.label("AC power: disconnected"),
                        None => ui.label("AC power: unknown"),
                    };
                    ui.colored_label(
                        snapshot_age_color(snapshot),
                        format!("Snapshot age: {}", snapshot_age_label(snapshot)),
                    );

                    if snapshot.batteries.is_empty() {
                        ui.label("Battery: none detected");
                    } else {
                        for battery in &snapshot.batteries {
                            let is_low = battery
                                .percentage
                                .is_some_and(|value| value <= LOW_BATTERY_THRESHOLD_PERCENT);
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&battery.name);
                                    ui.add_space(8.0);
                                    ui.colored_label(
                                        if is_low {
                                            egui::Color32::from_rgb(230, 90, 90)
                                        } else {
                                            battery_state_color(battery.state.as_deref())
                                        },
                                        battery_state_label(battery.state.as_deref()),
                                    );
                                    ui.add_space(8.0);
                                    if is_low {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(230, 90, 90),
                                            "Low",
                                        );
                                        ui.add_space(4.0);
                                    }
                                    ui.label(
                                        battery
                                            .percentage
                                            .map(|value| format!("{value}%"))
                                            .unwrap_or_else(|| "unknown".to_string()),
                                    );
                                });

                                if let Some(percentage) = battery.percentage {
                                    let fill = (percentage as f32 / 100.0).clamp(0.0, 1.0);
                                    ui.add(
                                        egui::ProgressBar::new(fill)
                                            .fill(if is_low {
                                                egui::Color32::from_rgb(230, 90, 90)
                                            } else {
                                                battery_state_color(battery.state.as_deref())
                                            })
                                            .desired_width(220.0)
                                            .show_percentage(),
                                    );
                                } else {
                                    ui.label("No percentage reported");
                                }
                            });
                        }
                    }

                    if snapshot.is_low_battery(LOW_BATTERY_THRESHOLD_PERCENT) {
                        ui.label("Battery warning: low");
                    }

                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        status_legend_chip(ui, "Charging", battery_state_color(Some("charging")));
                        status_legend_chip(
                            ui,
                            "Discharging",
                            battery_state_color(Some("discharging")),
                        );
                        status_legend_chip(ui, "Low", egui::Color32::from_rgb(230, 90, 90));
                    });
                }
                None => {
                    ui.label("Active performance profile unavailable");
                    ui.label("AC power: unknown");
                    ui.label("Battery: unavailable");
                }
            }
        });

        ui.add_space(12.0);

        let mut changed = false;

        ui.group(|ui| {
            ui.heading("Idle");
            ui.add_space(8.0);

            let mut blank_enabled = self.power.blank_screen_minutes.is_some();
            if ui
                .checkbox(&mut blank_enabled, "Blank screen after idle")
                .changed()
            {
                self.power.blank_screen_minutes = if blank_enabled {
                    Some(self.power.blank_screen_minutes.unwrap_or(10))
                } else {
                    None
                };
                changed = true;
            }

            if let Some(minutes) = &mut self.power.blank_screen_minutes {
                ui.add_enabled_ui(blank_enabled, |ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(minutes, 1..=120)
                                .text("Blank after (minutes)")
                                .clamping(egui::SliderClamping::Always),
                        )
                        .changed();
                });
            }

            ui.add_space(8.0);

            let mut suspend_enabled = self.power.suspend_minutes.is_some();
            if ui
                .checkbox(&mut suspend_enabled, "Suspend after idle")
                .changed()
            {
                self.power.suspend_minutes = if suspend_enabled {
                    Some(self.power.suspend_minutes.unwrap_or(15))
                } else {
                    None
                };
                changed = true;
            }

            if let Some(minutes) = &mut self.power.suspend_minutes {
                ui.add_enabled_ui(suspend_enabled, |ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(minutes, 1..=240)
                                .text("Suspend after (minutes)")
                                .clamping(egui::SliderClamping::Always),
                        )
                        .changed();
                });
            }
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Actions");
            ui.add_space(8.0);

            changed |= combo_box(
                ui,
                "Power button",
                &mut self.power.power_button_action,
                power_button_action_label,
                &[
                    PowerButtonAction::ShowPowerMenu,
                    PowerButtonAction::Suspend,
                    PowerButtonAction::PowerOff,
                    PowerButtonAction::DoNothing,
                ],
            );

            changed |= combo_box(
                ui,
                "Lid close",
                &mut self.power.lid_close_action,
                lid_close_action_label,
                &[
                    LidCloseAction::Suspend,
                    LidCloseAction::BlankScreen,
                    LidCloseAction::LockScreen,
                    LidCloseAction::DoNothing,
                ],
            );

            changed |= combo_box(
                ui,
                "Low battery",
                &mut self.power.low_battery_action,
                low_battery_action_label,
                &[
                    LowBatteryAction::NotifyOnly,
                    LowBatteryAction::Suspend,
                    LowBatteryAction::Hibernate,
                    LowBatteryAction::PowerOff,
                ],
            );

            changed |= combo_box(
                ui,
                "Performance mode",
                &mut self.power.performance_mode,
                performance_mode_label,
                &[
                    PerformanceMode::Balanced,
                    PerformanceMode::Performance,
                    PerformanceMode::PowerSaver,
                ],
            );
        });

        if changed {
            let mut settings = load_settings();
            settings.power = self.power.clone();
            if save_settings(&settings).is_ok() {
                actions.push(UiAction::ReloadSettings);
                self.refresh_power_status_now();
            }
        }
    }

    fn debug_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Debug");
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.heading("Diagnostics");
            ui.label(format!("Session type: {}", session_type_label()));
            ui.label(format!(
                "Build profile: {}",
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                }
            ));
            ui.label(format!(
                "Log file: {}",
                existing_focaldesk_log_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not found".to_string())
            ));

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Open log file").clicked() {
                    if let Some(path) = existing_focaldesk_log_path() {
                        self.debug_status = match open_path(&path) {
                            Ok(()) => "Opened log file".to_string(),
                            Err(err) => err,
                        };
                    } else {
                        self.debug_status = "No FocalDesk log file was found".to_string();
                    }
                }

                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(diagnostics_text());
                    self.debug_status = "Diagnostics copied to clipboard".to_string();
                }
            });

            ui.add_space(6.0);
            ui.label(&self.debug_status);
        });
    }

    fn ai_permissions_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("AI Permissions");
        ui.add_space(8.0);
        ui.label("Stored AI permissions live on disk and can be revoked here.");
        ui.add_space(8.0);

        match list_ai_permission_records() {
            Ok(records) if records.is_empty() => {
                ui.group(|ui| {
                    ui.label("No saved AI permissions yet.");
                });
            }
            Ok(records) => {
                for record in records {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(ai_permission_heading(&record));
                                ui.label(ai_permission_details(&record));
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Revoke").clicked() {
                                        self.ai_permissions_status = revoke_ai_permission(&record)
                                            .map(|_| "AI permission revoked".to_string())
                                            .unwrap_or_else(|err| err.to_string());
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(8.0);
                }
            }
            Err(err) => {
                ui.group(|ui| {
                    ui.label(format!("Unable to load AI permissions: {err}"));
                });
            }
        }

        if !self.ai_permissions_status.is_empty() {
            ui.add_space(8.0);
            ui.label(&self.ai_permissions_status);
        }
    }

    fn about_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("About");
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.heading("FocalDesk");
            ui.label("A Rust desktop environment built on Smithay and GTK4");
            ui.separator();
            ui.label(format!("Version: {}", env!("CARGO_PKG_VERSION")));
            ui.label(format!(
                "Build hash: {}",
                option_env!("VERGEN_GIT_SHA").unwrap_or("development")
            ));
            ui.label("Status: Early alpha");
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Session");
            ui.label(format!("Session type: {}", session_type_label()));
            ui.label(format!(
                "Build profile: {}",
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                }
            ));
            ui.label(format!(
                "Config path: {}",
                focaldesk_config_path().display()
            ));
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Project");
            ui.label("License: See LICENSE");
            ui.hyperlink_to(
                "Source code and issue tracking",
                "https://github.com/sjweiler/focaldesk",
            );
            ui.label("Credits: Smithay, GTK4, PipeWire, Rust");
        });
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            self.was_open = false;
            return;
        }

        if !self.was_open {
            self.reload_from_disk();
        }
        self.was_open = true;

        let mut open = self.open;
        let mut close_requested = false;
        let response = egui::Window::new("FocalDesk Settings")
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .default_size(egui::vec2(900.0, 430.0))
            .resizable(true)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Settings");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.set_height(ui.available_height());

                    ui.vertical(|ui| {
                        ui.set_width(180.0);
                        self.sidebar(ui);
                    });

                    ui.separator();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(12.0);
                            match self.tab {
                                SettingsPage::Appearance => self.appearance_page(ui),
                                SettingsPage::Network => self.network_page(ui),
                                SettingsPage::Bluetooth => self.bluetooth_page(ui),
                                SettingsPage::Displays => self.displays_page(ui),
                                SettingsPage::Workspaces => self.show_placeholder(ui, "Workspaces"),
                                SettingsPage::Keyboard => self.show_placeholder(ui, "Keyboard"),
                                SettingsPage::Privacy => self.show_placeholder(ui, "Privacy"),
                                SettingsPage::AiPermissions => self.ai_permissions_page(ui),
                                SettingsPage::Power => self.power_page(ui, actions),
                                SettingsPage::Debug => self.debug_page(ui),
                                SettingsPage::About => self.about_page(ui),
                            }
                        });
                });
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }

    fn show_placeholder(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.heading(title);
        ui.label(format!("{title} settings"));
    }
}

fn ai_permission_heading(record: &AiPermissionRecord) -> String {
    format!(
        "{} {}",
        permission_decision_label(record.decision),
        permission_resource_label(record.resource)
    )
}

fn ai_permission_details(record: &AiPermissionRecord) -> String {
    format!(
        "App: {} | Target: {} | Scope: {} | Updated: {}",
        record.app_identity,
        permission_target_label(&record.target),
        permission_scope_label(record.scope),
        format_system_time(record.updated_at)
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

fn permission_resource_label(resource: focaldesk_permissions::PermissionResource) -> &'static str {
    match resource {
        focaldesk_permissions::PermissionResource::Screenshot => "Screenshot",
        focaldesk_permissions::PermissionResource::Screencast => "Screencast",
        focaldesk_permissions::PermissionResource::ScreenShareWindow => "Window share",
        focaldesk_permissions::PermissionResource::ScreenShareOutput => "Output share",
        focaldesk_permissions::PermissionResource::AiChat => "AI chat",
        focaldesk_permissions::PermissionResource::Microphone => "Microphone",
        focaldesk_permissions::PermissionResource::Camera => "Camera",
        focaldesk_permissions::PermissionResource::ClipboardRead => "Clipboard read",
        focaldesk_permissions::PermissionResource::ClipboardWrite => "Clipboard write",
        focaldesk_permissions::PermissionResource::RemoteInput => "Remote input",
        focaldesk_permissions::PermissionResource::Notifications => "Notifications",
        focaldesk_permissions::PermissionResource::FileOpen => "File open",
        focaldesk_permissions::PermissionResource::FileSave => "File save",
    }
}

fn combo_box<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    label_fn: fn(T) -> &'static str,
    options: &[T],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(label)
            .selected_text(label_fn(*value))
            .show_ui(ui, |ui| {
                for option in options {
                    changed |= ui
                        .selectable_value(value, *option, label_fn(*option))
                        .changed();
                }
            });
    });
    changed
}

fn power_button_action_label(action: PowerButtonAction) -> &'static str {
    match action {
        PowerButtonAction::ShowPowerMenu => "Show power menu",
        PowerButtonAction::Suspend => "Suspend",
        PowerButtonAction::PowerOff => "Power off",
        PowerButtonAction::DoNothing => "Do nothing",
    }
}

fn lid_close_action_label(action: LidCloseAction) -> &'static str {
    match action {
        LidCloseAction::Suspend => "Suspend",
        LidCloseAction::BlankScreen => "Blank screen",
        LidCloseAction::LockScreen => "Lock screen",
        LidCloseAction::DoNothing => "Do nothing",
    }
}

fn low_battery_action_label(action: LowBatteryAction) -> &'static str {
    match action {
        LowBatteryAction::NotifyOnly => "Notify only",
        LowBatteryAction::Suspend => "Suspend",
        LowBatteryAction::Hibernate => "Hibernate",
        LowBatteryAction::PowerOff => "Power off",
    }
}

fn performance_mode_label(mode: PerformanceMode) -> &'static str {
    match mode {
        PerformanceMode::Balanced => "Balanced",
        PerformanceMode::Performance => "Performance",
        PerformanceMode::PowerSaver => "Power saver",
    }
}

fn performance_mode_profile_name(mode: PerformanceMode) -> &'static str {
    match mode {
        PerformanceMode::Balanced => "balanced",
        PerformanceMode::Performance => "performance",
        PerformanceMode::PowerSaver => "power-saver",
    }
}

fn battery_state_label(state: Option<&str>) -> &'static str {
    match state.map(|value| value.to_ascii_lowercase()) {
        Some(ref value) if value == "charging" => "Charging",
        Some(ref value) if value == "discharging" => "Discharging",
        Some(ref value) if value == "full" => "Full",
        Some(ref value) if value == "empty" => "Empty",
        Some(ref value) if value == "pending-charge" => "Pending charge",
        Some(ref value) if value == "pending-discharge" => "Pending discharge",
        _ => "Unknown",
    }
}

fn battery_state_color(state: Option<&str>) -> egui::Color32 {
    match state.map(|value| value.to_ascii_lowercase()) {
        Some(ref value) if value == "charging" => egui::Color32::from_rgb(80, 180, 120),
        Some(ref value) if value == "discharging" => egui::Color32::from_rgb(220, 180, 80),
        Some(ref value) if value == "full" => egui::Color32::from_rgb(100, 190, 255),
        Some(ref value) if value == "empty" => egui::Color32::from_rgb(230, 90, 90),
        Some(ref value) if value == "pending-charge" || value == "pending-discharge" => {
            egui::Color32::from_rgb(180, 160, 255)
        }
        _ => egui::Color32::from_rgb(180, 180, 180),
    }
}

fn status_legend_chip(ui: &mut egui::Ui, label: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.colored_label(color, "■");
        ui.label(label);
    });
}

fn format_system_time(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let seconds = duration.as_secs();
            let nanos = duration.subsec_nanos();
            format!("{seconds}.{nanos:09}s since epoch")
        }
        Err(_) => "before epoch".to_string(),
    }
}
