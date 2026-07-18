//! BlueZ integration via `bluetoothctl`, matching the shell-out-to-CLI
//! convention used by [`focaldesk_audio`] (pactl/wpctl) and
//! [`focaldesk_power`] (loginctl) rather than a raw D-Bus binding.

use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothSnapshot {
    pub powered: bool,
    pub scanning: bool,
    pub devices: Vec<BluetoothDevice>,
    pub error: Option<String>,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Full state: power, and every paired-or-visible device with its
/// connected/paired flags resolved via a per-device `info` lookup.
/// `scanning` is echoed back as given since `bluetoothctl` has no query for
/// "is a scan currently running" — callers track that themselves.
pub fn load_snapshot(scanning: bool) -> BluetoothSnapshot {
    let show = match run_bluetoothctl(&["show"]) {
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

    let powered = info_value(&show, "Powered");
    // `bluetoothctl paired-devices` was removed from BlueZ; `devices Paired`
    // is the current filtered form (`bluetoothctl help devices`).
    let paired_output = run_bluetoothctl(&["devices", "Paired"]).unwrap_or_default();
    let all_output = run_bluetoothctl(&["devices"]).unwrap_or_default();

    let mut devices = parse_devices(&paired_output, true);
    for device in parse_devices(&all_output, false) {
        if !devices.iter().any(|known| known.address == device.address) {
            devices.push(device);
        }
    }

    for device in &mut devices {
        if let Ok(info) = run_bluetoothctl(&["info", &device.address]) {
            device.connected = info_value(&info, "Connected");
            device.paired = device.paired || info_value(&info, "Paired");
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

pub fn set_power(enabled: bool) -> Result<String, String> {
    run_bluetoothctl(&["power", state_arg(enabled)])
}

pub fn set_scanning(enabled: bool) -> Result<String, String> {
    run_bluetoothctl(&["scan", state_arg(enabled)])
}

pub fn pair(address: &str) -> Result<String, String> {
    run_bluetoothctl(&["pair", address])
}

pub fn connect(address: &str) -> Result<String, String> {
    run_bluetoothctl(&["connect", address])
}

pub fn disconnect(address: &str) -> Result<String, String> {
    run_bluetoothctl(&["disconnect", address])
}

pub fn trust(address: &str) -> Result<String, String> {
    run_bluetoothctl(&["trust", address])
}

/// Unpairs and forgets `address`, wiping its stored link key so it stops
/// showing as paired and must be paired again to reconnect.
pub fn remove(address: &str) -> Result<String, String> {
    run_bluetoothctl(&["remove", address])
}

fn state_arg(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn parse_devices(output: &str, paired: bool) -> Vec<BluetoothDevice> {
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

fn info_value(info: &str, key: &str) -> bool {
    info.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix(key)
            .and_then(|value| value.trim().strip_prefix(':'))
            .map(|value| value.trim() == "yes")
            .unwrap_or(false)
    })
}

fn run_bluetoothctl(args: &[&str]) -> Result<String, String> {
    run_bluetoothctl_with_timeout(args, DEFAULT_TIMEOUT)
}

fn run_bluetoothctl_with_timeout(args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("bluetoothctl")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("bluetoothctl: {err}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "bluetoothctl: command timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("bluetoothctl: {err}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|err| format!("bluetoothctl: {err}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
        Err(format!("bluetoothctl: {message}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices_reads_address_and_name() {
        let output =
            "Device AA:BB:CC:DD:EE:FF Wireless Headphones\nDevice 11:22:33:44:55:66 Keyboard\n";
        let devices = parse_devices(output, true);

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(devices[0].name, "Wireless Headphones");
        assert!(devices[0].paired);
        assert!(!devices[0].connected);
    }

    #[test]
    fn parse_devices_falls_back_to_unknown_device_name() {
        let devices = parse_devices("Device AA:BB:CC:DD:EE:FF\n", false);
        assert_eq!(devices[0].name, "Unknown Device");
    }

    #[test]
    fn parse_devices_ignores_non_device_lines() {
        let devices = parse_devices("Controller AA:BB:CC:DD:EE:FF localhost\n", true);
        assert!(devices.is_empty());
    }

    #[test]
    fn info_value_reads_yes_no_fields() {
        let info = "Device AA:BB:CC:DD:EE:FF\n\tPowered: yes\n\tPaired: no\n";
        assert!(info_value(info, "Powered"));
        assert!(!info_value(info, "Paired"));
    }

    #[test]
    fn info_value_is_false_when_key_absent() {
        assert!(!info_value("Device AA:BB:CC:DD:EE:FF\n", "Connected"));
    }
}
