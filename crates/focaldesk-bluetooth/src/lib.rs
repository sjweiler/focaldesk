//! BlueZ integration via `bluetoothctl`, matching the shell-out-to-CLI
//! convention used by [`focaldesk_audio`] (pactl/wpctl) and
//! [`focaldesk_power`] (loginctl) rather than a raw D-Bus binding.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

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

struct DiscoverySession {
    connection: Connection,
    adapter_path: OwnedObjectPath,
}

static DISCOVERY_SESSION: OnceLock<Mutex<Option<DiscoverySession>>> = OnceLock::new();

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
            device.paired =
                device.paired || info_value(&info, "Paired") || info_value(&info, "Bonded");
            if let Some(name) = info_text_value(&info, "Name")
                .or_else(|| info_text_value(&info, "Alias"))
                .filter(|name| !is_address_derived_name(name, &device.address))
            {
                device.name = name;
            }
        }
    }

    // BlueZ uses a hyphenated random address as the alias for nameless BLE
    // advertisements (Nearby/continuity beacons are common examples). They
    // are not useful pairing targets and otherwise appear as rows of hex.
    // Never hide something the user has already paired or connected.
    devices.retain(|device| {
        device.paired || device.connected || !is_address_derived_name(&device.name, &device.address)
    });

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
    let session = DISCOVERY_SESSION.get_or_init(|| Mutex::new(None));
    let mut session = session
        .lock()
        .map_err(|_| "BlueZ discovery session lock was poisoned".to_string())?;

    if enabled {
        if session.is_some() {
            return Ok("Bluetooth discovery is already running".to_string());
        }

        let connection = Connection::system().map_err(|err| format!("connect to BlueZ: {err}"))?;
        let adapter_path = bluez_adapter_path(&connection)?;
        {
            let proxy = bluez_adapter_proxy(&connection, &adapter_path)?;
            proxy
                .call::<_, _, ()>("StartDiscovery", &())
                .map_err(|err| format!("start Bluetooth discovery: {err}"))?;
        }

        *session = Some(DiscoverySession {
            connection,
            adapter_path,
        });
        Ok("Bluetooth discovery started".to_string())
    } else {
        let Some(active) = session.take() else {
            return Ok("Bluetooth discovery is already stopped".to_string());
        };
        let proxy = bluez_adapter_proxy(&active.connection, &active.adapter_path)?;
        proxy
            .call::<_, _, ()>("StopDiscovery", &())
            .map_err(|err| format!("stop Bluetooth discovery: {err}"))?;
        Ok("Bluetooth discovery stopped".to_string())
    }
}

fn bluez_adapter_path(connection: &Connection) -> Result<OwnedObjectPath, String> {
    let manager = zbus::blocking::fdo::ObjectManagerProxy::builder(connection)
        .destination("org.bluez")
        .map_err(|err| format!("find BlueZ service: {err}"))?
        .path("/")
        .map_err(|err| format!("find BlueZ object manager: {err}"))?
        .build()
        .map_err(|err| format!("connect to BlueZ object manager: {err}"))?;
    let objects = manager
        .get_managed_objects()
        .map_err(|err| format!("list BlueZ adapters: {err}"))?;

    objects
        .into_iter()
        .find_map(|(path, interfaces)| {
            interfaces
                .keys()
                .any(|name| name.as_str() == "org.bluez.Adapter1")
                .then_some(path)
        })
        .ok_or_else(|| "BlueZ has no Bluetooth adapter".to_string())
}

fn bluez_adapter_proxy<'a>(
    connection: &'a Connection,
    adapter_path: &'a OwnedObjectPath,
) -> Result<Proxy<'a>, String> {
    Proxy::new(
        connection,
        "org.bluez",
        adapter_path.as_str(),
        "org.bluez.Adapter1",
    )
    .map_err(|err| format!("connect to BlueZ adapter: {err}"))
}

pub fn pair(address: &str) -> Result<String, String> {
    let mut child = Command::new("bluetoothctl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("bluetoothctl: {err}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "bluetoothctl: pairing agent has no input".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bluetoothctl: pairing agent has no output".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "bluetoothctl: pairing agent has no error output".to_string())?;

    let (tx, rx) = mpsc::channel();
    stream_bluetoothctl_output(stdout, tx.clone());
    stream_bluetoothctl_output(stderr, tx);

    send_bluetoothctl_command(&mut stdin, "agent DisplayYesNo")?;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum PairStage {
        RegisterAgent,
        DefaultAgent,
        Pair,
    }

    let mut stage = PairStage::RegisterAgent;
    let mut transcript = String::new();
    let mut confirmation_sent = false;
    let mut pairing_succeeded_at = None;
    let mut bonded_since = None;
    let mut default_agent_requested_at = None;
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if Instant::now() >= deadline {
            stop_bluetoothctl_child(&mut child, &mut stdin);
            return Err(
                "Bluetooth pairing timed out. Keep the device in pairing mode and try again."
                    .to_string(),
            );
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => transcript.push_str(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_bluetoothctl_child(&mut child, &mut stdin);
                return Err("Bluetooth pairing agent stopped unexpectedly".to_string());
            }
        }

        let lower = transcript.to_ascii_lowercase();
        if stage == PairStage::RegisterAgent
            && (lower.contains("agent registered") || lower.contains("already registered"))
        {
            send_bluetoothctl_command(&mut stdin, "default-agent")?;
            stage = PairStage::DefaultAgent;
            default_agent_requested_at = Some(Instant::now());
            transcript.clear();
            continue;
        }

        if stage == PairStage::DefaultAgent
            && default_agent_requested_at
                .is_some_and(|requested_at| requested_at.elapsed() >= Duration::from_millis(300))
        {
            send_bluetoothctl_command(&mut stdin, &format!("pair {address}"))?;
            stage = PairStage::Pair;
            transcript.clear();
            continue;
        }

        if stage == PairStage::Pair
            && !confirmation_sent
            && (lower.contains("confirm passkey") || lower.contains("yes/no"))
        {
            send_bluetoothctl_command(&mut stdin, "yes")?;
            confirmation_sent = true;
        }

        if stage == PairStage::Pair
            && pairing_succeeded_at.is_none()
            && lower.contains("pairing successful")
        {
            pairing_succeeded_at = Some(Instant::now());
        }

        if pairing_succeeded_at.is_some()
            && let Ok(info) = run_bluetoothctl(&["info", address])
        {
            let fully_bonded = info_value(&info, "Paired") && info_value(&info, "Bonded");
            if fully_bonded {
                let stable_since = bonded_since.get_or_insert_with(Instant::now);
                if stable_since.elapsed() < Duration::from_secs(2) {
                    continue;
                }
                stop_bluetoothctl_child(&mut child, &mut stdin);
                return Ok("Pairing successful".to_string());
            } else {
                bonded_since = None;
            }
        }

        if stage == PairStage::Pair && bluetoothctl_reported_failure(&transcript) {
            let message = transcript
                .lines()
                .rev()
                .find(|line| bluetoothctl_reported_failure(line))
                .unwrap_or(transcript.trim());
            let error = friendly_bluetooth_error(message);
            stop_bluetoothctl_child(&mut child, &mut stdin);
            return Err(error);
        }
    }
}

fn stream_bluetoothctl_output<R>(mut stream: R, tx: mpsc::Sender<String>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 512];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if tx
                        .send(String::from_utf8_lossy(&buffer[..read]).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

fn send_bluetoothctl_command(stdin: &mut impl Write, command: &str) -> Result<(), String> {
    writeln!(stdin, "{command}").map_err(|err| format!("bluetoothctl: {err}"))?;
    stdin.flush().map_err(|err| format!("bluetoothctl: {err}"))
}

fn stop_bluetoothctl_child(child: &mut std::process::Child, stdin: &mut impl Write) {
    let _ = send_bluetoothctl_command(stdin, "quit");
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn pair_and_connect(address: &str) -> Result<String, String> {
    pair(address)?;
    trust(address)?;

    let info = run_bluetoothctl(&["info", address])?;
    if !info_value(&info, "Connected") {
        connect(address)?;
    }

    Ok("Paired, trusted, and connected".to_string())
}

pub fn connect(address: &str) -> Result<String, String> {
    const ATTEMPTS: usize = 3;

    for attempt in 1..=ATTEMPTS {
        match run_bluetoothctl_with_timeout(&["connect", address], Duration::from_secs(15)) {
            Ok(output) => return Ok(output),
            Err(err) if attempt < ATTEMPTS && is_transient_connection_error(&err) => {
                // Headsets commonly need a moment to become page-able after
                // their case opens. Keep this retry here so every FocalDesk
                // caller gets the same reconnect behavior.
                thread::sleep(Duration::from_millis(750));
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("the connection loop always returns on its final attempt")
}

pub fn disconnect(address: &str) -> Result<String, String> {
    run_bluetoothctl_with_timeout(&["disconnect", address], Duration::from_secs(10))
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

fn info_text_value(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(key)
            .and_then(|value| value.trim().strip_prefix(':'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn is_address_derived_name(name: &str, address: &str) -> bool {
    let normalized_name: String = name
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .flat_map(char::to_uppercase)
        .collect();
    let normalized_address: String = address
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .flat_map(char::to_uppercase)
        .collect();

    normalized_name.len() == 12 && normalized_name == normalized_address
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

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if output.status.success() && !bluetoothctl_reported_failure(&stdout) {
        Ok(stdout)
    } else {
        let message = if stderr.is_empty() { stdout } else { stderr };
        Err(friendly_bluetooth_error(&message))
    }
}

fn bluetoothctl_reported_failure(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        line.starts_with("failed to")
            || line.contains("not available")
            || line.contains("authentication failed")
            || line.contains("no default controller")
    })
}

fn is_transient_connection_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("br-connection-page-timeout")
        || lower.contains("br-connection-create-socket")
        || lower.contains("br-connection-unknown")
        || lower.contains("did not answer")
        || lower.contains("not available")
        || lower.contains("not currently available")
        || lower.contains("command timed out")
}

fn friendly_bluetooth_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("br-connection-page-timeout") {
        "Bluetooth device did not answer. Make sure it is awake, nearby, and not connected to another host, then try Connect again."
            .to_string()
    } else if lower.contains("br-connection-unknown") || lower.contains("not available") {
        "Bluetooth device is not currently available. Make sure it is awake and nearby, then try again."
            .to_string()
    } else if lower.contains("authentication") {
        format!("Bluetooth pairing authentication failed: {message}")
    } else {
        format!("bluetoothctl: {message}")
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

    #[test]
    fn info_text_value_reads_name_and_alias() {
        let info = "Device AA:BB:CC:DD:EE:FF\n\tName: AirPods Pro\n\tAlias: Headphones\n";
        assert_eq!(
            info_text_value(info, "Name").as_deref(),
            Some("AirPods Pro")
        );
        assert_eq!(
            info_text_value(info, "Alias").as_deref(),
            Some("Headphones")
        );
    }

    #[test]
    fn recognizes_address_derived_bluez_aliases() {
        assert!(is_address_derived_name(
            "5F-AA-DA-68-CA-76",
            "5F:AA:DA:68:CA:76"
        ));
        assert!(is_address_derived_name(
            "5faa-da68-ca76",
            "5F:AA:DA:68:CA:76"
        ));
        assert!(!is_address_derived_name("MELK-OF214C", "BE:16:6B:00:3D:4C"));
    }

    #[test]
    fn recognizes_failures_reported_with_success_exit_status() {
        assert!(bluetoothctl_reported_failure(
            "Attempting to connect to 74:65:0C:1E:8F:BB\nFailed to connect: org.bluez.Error.Failed br-connection-unknown"
        ));
        assert!(bluetoothctl_reported_failure(
            "Device 74:65:0C:1E:8F:BB not available"
        ));
        assert!(!bluetoothctl_reported_failure("Connection successful"));
    }

    #[test]
    fn recognizes_transient_headset_connection_failures() {
        assert!(is_transient_connection_error(
            "bluetoothctl: Failed to connect: org.bluez.Error.Failed br-connection-page-timeout"
        ));
        assert!(is_transient_connection_error(
            "Bluetooth device is not currently available"
        ));
        assert!(!is_transient_connection_error(
            "Bluetooth pairing authentication failed"
        ));
    }

    #[test]
    fn page_timeout_error_explains_how_to_make_headset_reachable() {
        assert_eq!(
            friendly_bluetooth_error(
                "Failed to connect: org.bluez.Error.Failed br-connection-page-timeout"
            ),
            "Bluetooth device did not answer. Make sure it is awake, nearby, and not connected to another host, then try Connect again."
        );
    }

    #[test]
    #[ignore = "requires a live BlueZ system bus"]
    fn live_discovery_session_filters_address_only_devices() {
        set_scanning(true).expect("start BlueZ discovery");
        let scanning = run_bluetoothctl(&["show"]).expect("query BlueZ controller");
        assert!(info_value(&scanning, "Discovering"));
        let snapshot = load_snapshot(true);
        assert!(
            snapshot.devices.iter().all(|device| {
                device.paired
                    || device.connected
                    || !is_address_derived_name(&device.name, &device.address)
            }),
            "snapshot exposed a BlueZ address-derived alias: {snapshot:#?}"
        );

        set_scanning(false).expect("stop BlueZ discovery");
    }

    #[test]
    #[ignore = "pairs the device in FOCALDESK_TEST_BLUETOOTH_ADDRESS"]
    fn live_pairing_remains_bonded() {
        let address = std::env::var("FOCALDESK_TEST_BLUETOOTH_ADDRESS")
            .expect("set FOCALDESK_TEST_BLUETOOTH_ADDRESS");
        println!("pair result: {:?}", pair_and_connect(&address));

        let mut remained_bonded = true;
        for sample in 0..10 {
            thread::sleep(Duration::from_millis(500));
            let info = run_bluetoothctl(&["info", &address]).unwrap_or_default();
            let paired = info_value(&info, "Paired");
            let bonded = info_value(&info, "Bonded");
            let trusted = info_value(&info, "Trusted");
            let connected = info_value(&info, "Connected");
            println!(
                "sample {sample}: paired={paired} bonded={bonded} trusted={trusted} connected={connected}"
            );
            remained_bonded &= paired || bonded;
        }

        assert!(remained_bonded, "BlueZ did not retain the pairing bond");
    }
}
