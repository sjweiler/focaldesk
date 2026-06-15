use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;
use focaldesk_config::{FocalDeskConfig, save_config};
use std::collections::HashMap;
use std::process::Command;

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
    tab: SettingsPage,
    config: FocalDeskConfig,
    wifi_passwords: HashMap<String, String>,
    network_status: String,
    bluetooth_status: String,
    bluetooth_scanning: bool,
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
    Power,
    Debug,
    About,
}

impl Default for SettingsPanel {
    fn default() -> Self {
        Self {
            tab: SettingsPage::Appearance,
            config: FocalDeskConfig::default(),
            wifi_passwords: HashMap::new(),
            network_status: String::new(),
            bluetooth_status: String::new(),
            bluetooth_scanning: false,
            open: false,
        }
    }
}

impl SettingsPanel {
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

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

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
                                SettingsPage::Power => self.show_placeholder(ui, "Power"),
                                SettingsPage::Debug => self.show_placeholder(ui, "Debug"),
                                SettingsPage::About => self.show_placeholder(ui, "About"),
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
