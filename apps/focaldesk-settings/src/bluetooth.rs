//! BlueZ UI lifecycle, with explicit pairing replies and bounded discovery.
use crate::{
    control_process::{run, LiveProcess},
    network::valid_mac,
};
use focaldesk_bluetooth::{BluetoothDevice, BluetoothSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::Mutex,
    time::{Duration, Instant},
};

fn ctl(args: &[&str]) -> Result<String, String> {
    let output = run("bluetoothctl", args, None, Duration::from_secs(8))?;
    let lower = output.to_ascii_lowercase();
    if lower.contains("failed to")
        || lower.contains("no default controller")
        || lower.contains("not available")
    {
        return Err(output.chars().take(512).collect());
    }
    Ok(output)
}
fn listed(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("Device ")?;
            let (address, name) = rest.split_once(' ')?;
            valid_mac(address).then(|| (address.to_owned(), name.to_owned()))
        })
        .take(256)
        .collect()
}
fn inventory(scanning: bool) -> Result<BluetoothSnapshot, String> {
    let show = ctl(&["show"])?;
    let paired: BTreeSet<_> = listed(&ctl(&["devices", "Paired"])?)
        .into_iter()
        .map(|(address, _)| address)
        .collect();
    let connected: BTreeSet<_> = listed(&ctl(&["devices", "Connected"])?)
        .into_iter()
        .map(|(address, _)| address)
        .collect();
    let mut devices: Vec<_> = listed(&ctl(&["devices"])?)
        .into_iter()
        .map(|(address, name)| BluetoothDevice {
            paired: paired.contains(&address),
            connected: connected.contains(&address),
            address,
            name,
        })
        .collect();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.cmp(&b.name))
    });
    Ok(BluetoothSnapshot {
        powered: show.lines().any(|line| line.trim() == "Powered: yes"),
        scanning,
        devices,
        error: None,
    })
}
#[derive(Clone, Serialize)]
pub struct Pairing {
    pub address: String,
    pub phase: String,
    pub prompt: Option<String>,
    pub kind: Option<String>,
    pub message: String,
}
#[derive(Serialize)]
pub struct BluetoothView {
    pub lease: u64,
    pub snapshot: BluetoothSnapshot,
    pub pairing: Option<Pairing>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BluetoothAction {
    SetPower { enabled: bool },
    Scan,
    StopScan,
    Pair { address: String },
    Reply { value: String },
    CancelPairing,
    Connect { address: String },
    Disconnect { address: String },
    Forget { address: String },
}
struct Session {
    process: LiveProcess,
    pairing: Option<Pairing>,
    stage: u8,
    paired_at: Option<Instant>,
}
#[derive(Default)]
struct State {
    lease: u64,
    active: bool,
    session: Option<Session>,
    last_pairing: Option<Pairing>,
}
#[derive(Default)]
pub struct BluetoothService {
    state: Mutex<State>,
}

fn clean(value: &str) -> String {
    let mut result = String::new();
    let mut escape = false;
    for ch in value.chars() {
        if ch == '\u{1b}' {
            escape = true;
        } else if escape {
            if ch.is_ascii_alphabetic() {
                escape = false;
            }
        } else if !ch.is_control() || ch == '\n' {
            result.push(ch);
        }
    }
    result
}
fn advance(state: &mut State) -> Result<(), String> {
    let Some(session) = state.session.as_mut() else {
        return Ok(());
    };
    let process = session.process.state();
    let text = clean(&process.text);
    let lower = text.to_ascii_lowercase();
    if let Some(pairing) = session.pairing.as_mut() {
        if process.done
            || lower.contains("failed to")
            || lower.contains("authenticationcanceled")
            || lower.contains("authenticationrejected")
        {
            pairing.phase = "failed".into();
            pairing.message = process.error.unwrap_or_else(|| {
                "Pairing ended or was rejected. Try again with the device in pairing mode.".into()
            });
            state.last_pairing = Some(pairing.clone());
            state.session = None;
            return Ok(());
        }
        if session.stage == 0
            && (lower.contains("agent registered") || lower.contains("already registered"))
        {
            session.process.send("default-agent")?;
            session.stage = 1;
            session.process.clear();
        } else if session.stage == 1 && lower.contains("default agent request successful") {
            session.process.send(&format!("pair {}", pairing.address))?;
            session.stage = 2;
            session.process.clear();
        } else if session.stage >= 2 {
            if lower.contains("pairing successful") {
                let since = *session.paired_at.get_or_insert_with(Instant::now);
                pairing.message = "Finishing the pairing handshake…".into();
                let info = ctl(&["info", &pairing.address])?;
                if since.elapsed() < Duration::from_secs(2)
                    || !info.lines().any(|line| line.trim() == "Paired: yes")
                    || !info.lines().any(|line| line.trim() == "Bonded: yes")
                {
                    return Ok(());
                }
                pairing.phase = "succeeded".into();
                pairing.message = "Paired. Choose Connect to use this device.".into();
                pairing.prompt = None;
                pairing.kind = None;
                state.last_pairing = Some(pairing.clone());
                state.session = None;
                return Ok(());
            }
            if pairing.prompt.is_none() {
                let kind = if lower.contains("confirm passkey") || lower.contains("(yes/no)") {
                    Some("confirm")
                } else if lower.contains("enter pin code") {
                    Some("pin")
                } else if lower.contains("enter passkey") {
                    Some("passkey")
                } else {
                    None
                };
                if let Some(kind) = kind {
                    pairing.phase = "waiting".into();
                    pairing.kind = Some(kind.into());
                    pairing.prompt = Some(
                        text.chars()
                            .rev()
                            .take(512)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect(),
                    );
                } else if lower.contains("passkey:") || lower.contains("pin code:") {
                    pairing.message = text
                        .chars()
                        .rev()
                        .take(512)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                }
            }
        }
    } else if process.done || lower.contains("failed to") || lower.contains("no default controller")
    {
        state.session = None;
        if lower.contains("failed to") || lower.contains("no default controller") {
            return Err(
                "Bluetooth discovery could not start. Check the adapter and try again.".into(),
            );
        }
    }
    Ok(())
}
fn view(state: &mut State) -> Result<BluetoothView, String> {
    advance(state)?;
    let pairing = state
        .session
        .as_ref()
        .and_then(|session| session.pairing.clone())
        .or_else(|| state.last_pairing.clone());
    let scanning = state
        .session
        .as_ref()
        .is_some_and(|session| session.pairing.is_none());
    Ok(BluetoothView {
        lease: state.lease,
        snapshot: inventory(scanning)?,
        pairing,
    })
}
fn check(state: &State, lease: u64) -> Result<(), String> {
    if state.active && state.lease == lease {
        Ok(())
    } else {
        Err("This Bluetooth page has closed. Reopen it to continue.".into())
    }
}
impl BluetoothService {
    pub fn open(&self) -> Result<BluetoothView, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Bluetooth controls unavailable.")?;
        state.session = None;
        state.last_pairing = None;
        state.lease = state.lease.wrapping_add(1);
        state.active = true;
        view(&mut state)
    }
    pub fn snapshot(&self, lease: u64) -> Result<BluetoothView, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Bluetooth controls unavailable.")?;
        check(&state, lease)?;
        view(&mut state)
    }
    pub fn close(&self, lease: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.lease == lease {
                state.active = false;
                state.session = None;
            }
        }
    }
    pub fn stop_all(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = false;
            state.session = None;
        }
    }
    pub fn apply(&self, lease: u64, action: BluetoothAction) -> Result<BluetoothView, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Bluetooth controls unavailable.")?;
        check(&state, lease)?;
        advance(&mut state)?;
        match action {
            BluetoothAction::Reply { value } => {
                let session = state
                    .session
                    .as_mut()
                    .ok_or("No pairing request is waiting.")?;
                let pairing = session
                    .pairing
                    .as_mut()
                    .ok_or("No pairing request is waiting.")?;
                let valid = match pairing.kind.as_deref() {
                    Some("confirm") => matches!(value.as_str(), "yes" | "no"),
                    Some("pin") => {
                        !value.is_empty()
                            && value.len() <= 16
                            && value.bytes().all(|b| b.is_ascii_graphic())
                    }
                    Some("passkey") => {
                        value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit())
                    }
                    _ => false,
                };
                if pairing.phase != "waiting" || !valid {
                    return Err("Invalid pairing response.".into());
                }
                session.process.clear();
                session.process.send(&value)?;
                pairing.phase = "pairing".into();
                pairing.prompt = None;
                pairing.kind = None;
            }
            BluetoothAction::CancelPairing | BluetoothAction::StopScan => {
                state.session = None;
                state.last_pairing = None;
            }
            BluetoothAction::SetPower { enabled } => {
                state.session = None;
                state.last_pairing = None;
                ctl(&["power", if enabled { "on" } else { "off" }])?;
            }
            BluetoothAction::Scan => {
                if !inventory(false)?.powered {
                    return Err("Turn on Bluetooth before scanning.".into());
                }
                state.session = None;
                state.last_pairing = None;
                let process = LiveProcess::start("bluetoothctl", Duration::from_secs(30))?;
                process.send("scan on")?;
                state.session = Some(Session {
                    process,
                    pairing: None,
                    stage: 0,
                    paired_at: None,
                });
            }
            action => {
                let address = match &action {
                    BluetoothAction::Pair { address }
                    | BluetoothAction::Connect { address }
                    | BluetoothAction::Disconnect { address }
                    | BluetoothAction::Forget { address } => address,
                    _ => unreachable!(),
                };
                if !valid_mac(address) {
                    return Err("Invalid Bluetooth device address.".into());
                }
                let snapshot = inventory(false)?;
                let device = snapshot
                    .devices
                    .iter()
                    .find(|device| device.address.eq_ignore_ascii_case(address))
                    .ok_or("This Bluetooth device disappeared. Refresh and try again.")?;
                if !snapshot.powered {
                    return Err("Turn on Bluetooth first.".into());
                }
                match &action {
                    BluetoothAction::Pair { .. } => {
                        if device.paired {
                            return Err("This device is already paired. Choose Connect.".into());
                        }
                        state.session = None;
                        state.last_pairing = None;
                        let process = LiveProcess::start("bluetoothctl", Duration::from_secs(60))?;
                        process.send("agent KeyboardDisplay")?;
                        state.session=Some(Session {process,stage:0,paired_at:None,pairing:Some(Pairing {address:device.address.clone(),phase:"pairing".into(),prompt:None,kind:None,message:"Keep the device in pairing mode. Compare any code before confirming.".into()})});
                    }
                    BluetoothAction::Connect { .. } => {
                        if !device.paired {
                            return Err("Pair this device before connecting.".into());
                        }
                        ctl(&["connect", &device.address])?;
                    }
                    BluetoothAction::Disconnect { .. } => {
                        ctl(&["disconnect", &device.address])?;
                    }
                    BluetoothAction::Forget { .. } => {
                        state.session = None;
                        ctl(&["remove", &device.address])?;
                    }
                    _ => unreachable!(),
                }
            }
        }
        view(&mut state)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_command_injection_in_device_lists_and_strips_terminal_codes() {
        assert!(listed("Device --help Bad\nDevice AA:BB:CC:DD:EE:FF Headset").len() == 1);
        assert_eq!(
            clean("\u{1b}[0;32mConfirm passkey 123456\u{1b}[0m"),
            "Confirm passkey 123456"
        );
    }
    #[test]
    fn closed_leases_cannot_mutate_a_new_page() {
        let state = State {
            lease: 2,
            active: true,
            ..State::default()
        };
        assert!(check(&state, 1).is_err());
        assert!(check(&state, 2).is_ok());
    }
}
