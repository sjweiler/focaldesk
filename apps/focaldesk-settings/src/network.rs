//! NetworkManager controls. The frontend supplies typed intent, never commands.
use crate::control_process::run;
use serde::{Deserialize, Serialize};
use std::{sync::Mutex, time::Duration};

#[derive(Clone, Serialize)]
pub struct NetworkDevice {
    pub name: String,
    pub kind: String,
    pub state: String,
    pub connection: String,
}
#[derive(Clone, Serialize)]
pub struct WifiNetwork {
    pub id: String,
    pub device: String,
    pub bssid: String,
    pub ssid: String,
    pub security: String,
    pub signal: u8,
    pub active: bool,
}
#[derive(Serialize)]
pub struct NetworkSnapshot {
    pub wifi_enabled: bool,
    pub devices: Vec<NetworkDevice>,
    pub networks: Vec<WifiNetwork>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkAction {
    SetWifi { enabled: bool },
    Rescan,
    ConnectWifi { id: String, password: String },
    SetDevice { device: String, connected: bool },
}
#[derive(Default)]
pub struct NetworkService {
    operation: Mutex<()>,
}

fn nm(args: &[&str], secret: Option<&str>, seconds: u64) -> Result<String, String> {
    run("nmcli", args, secret, Duration::from_secs(seconds))
}
fn fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            fields.last_mut().unwrap().push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ':' {
            fields.push(String::new());
        } else {
            fields.last_mut().unwrap().push(ch);
        }
    }
    fields
}
fn inventory() -> Result<NetworkSnapshot, String> {
    let wifi_enabled = nm(&["-t", "-f", "WIFI", "radio"], None, 3)? == "enabled";
    let devices = nm(
        &[
            "-t",
            "-f",
            "DEVICE,TYPE,STATE,CONNECTION",
            "device",
            "status",
        ],
        None,
        3,
    )?
    .lines()
    .filter_map(|line| {
        let f = fields(line);
        if f.len() != 4 || !matches!(f[1].as_str(), "ethernet" | "wifi") {
            return None;
        }
        Some(NetworkDevice {
            name: f[0].clone(),
            kind: f[1].clone(),
            state: f[2].clone(),
            connection: f[3].clone(),
        })
    })
    .take(256)
    .collect();
    let mut networks = Vec::new();
    if wifi_enabled {
        let listing = nm(
            &[
                "-t",
                "-f",
                "IN-USE,SSID,BSSID,SECURITY,SIGNAL,DEVICE",
                "device",
                "wifi",
                "list",
                "--rescan",
                "no",
            ],
            None,
            5,
        )?;
        for line in listing.lines().take(1024) {
            let f = fields(line);
            if f.len() != 6 || f[1].is_empty() {
                continue;
            }
            networks.push(WifiNetwork {
                id: format!("{}|{}", f[5], f[2]),
                device: f[5].clone(),
                bssid: f[2].clone(),
                ssid: f[1].clone(),
                security: f[3].clone(),
                signal: f[4].parse::<u8>().unwrap_or(0).min(100),
                active: f[0] == "*",
            });
        }
        networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    }
    Ok(NetworkSnapshot {
        wifi_enabled,
        devices,
        networks,
    })
}
impl NetworkService {
    pub fn snapshot(&self) -> Result<NetworkSnapshot, String> {
        let _lock = self
            .operation
            .lock()
            .map_err(|_| "Network controls unavailable.")?;
        inventory()
    }
    pub fn apply(&self, action: NetworkAction) -> Result<NetworkSnapshot, String> {
        let _lock = self
            .operation
            .lock()
            .map_err(|_| "Network controls unavailable.")?;
        let current = inventory()?;
        match action {
            NetworkAction::SetWifi { enabled } => {
                nm(
                    &["radio", "wifi", if enabled { "on" } else { "off" }],
                    None,
                    5,
                )?;
            }
            NetworkAction::Rescan => {
                if !current.wifi_enabled {
                    return Err("Turn on Wi-Fi before scanning.".into());
                }
                nm(&["--wait", "10", "device", "wifi", "rescan"], None, 12)?;
            }
            NetworkAction::SetDevice { device, connected } => {
                if device.is_empty()
                    || device.len() > 15
                    || device.starts_with('-')
                    || !device.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':')
                    })
                {
                    return Err("Invalid network device identifier.".into());
                }
                let target = current
                    .devices
                    .iter()
                    .find(|d| d.name == device)
                    .ok_or("This network device disconnected. Refresh and try again.")?;
                nm(
                    &[
                        "--wait",
                        "20",
                        "device",
                        if connected { "connect" } else { "disconnect" },
                        &target.name,
                    ],
                    None,
                    22,
                )?;
            }
            NetworkAction::ConnectWifi { id, password } => {
                let target = current
                    .networks
                    .iter()
                    .find(|n| n.id == id)
                    .ok_or("This access point disappeared. Scan and try again.")?;
                if !valid_mac(&target.bssid) || target.device.starts_with('-') {
                    return Err("Invalid access point identifier.".into());
                }
                if password.len() > 256 || password.contains(['\n', '\r', '\0']) {
                    return Err("Invalid Wi-Fi password.".into());
                }
                // --ask reads secrets on stdin instead of exposing them in argv.
                let result = nm(
                    &[
                        "--ask",
                        "--wait",
                        "20",
                        "device",
                        "wifi",
                        "connect",
                        &target.bssid,
                        "ifname",
                        &target.device,
                    ],
                    Some(&password),
                    22,
                );
                // Drop the password before the next inventory request.
                drop(password);
                result?;
            }
        }
        inventory()
    }
}
pub(crate) fn valid_mac(value: &str) -> bool {
    value.len() == 17
        && value.split(':').count() == 6
        && value
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_escaped_names_and_validates_identifiers() {
        assert_eq!(
            fields(r"*:Office\: Main:AA\:BB\:CC\:DD\:EE\:FF:WPA2:80:wlan0"),
            vec![
                "*",
                "Office: Main",
                "AA:BB:CC:DD:EE:FF",
                "WPA2",
                "80",
                "wlan0"
            ]
        );
        assert!(valid_mac("AA:BB:CC:DD:EE:FF"));
        for value in ["--help", "AA:BB:CC:DD:EE:GG", "AA:BB:CC:DD:EE:FF\nquit"] {
            assert!(!valid_mac(value));
        }
    }
}
