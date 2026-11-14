//! PipeWire-Pulse/PulseAudio controls. Only fixed commands and freshly resolved
//! device IDs reach pactl; frontend input never becomes a shell command.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Output,
    Input,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub label: String,
    pub kind: DeviceKind,
    pub volume_percent: f64,
    pub muted: bool,
    pub active_port: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSnapshot {
    pub server: String,
    pub outputs: Vec<AudioDevice>,
    pub inputs: Vec<AudioDevice>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioAction {
    SetDefault { device_id: String },
    SetVolume { device_id: String, percent: f64 },
    SetMute { device_id: String, muted: bool },
}

/// Injectable command boundary for deterministic tests. Production uses Pactl.
pub trait AudioCommands {
    fn run(&self, args: &[&str]) -> Result<String, String>;
}
pub struct Pactl;

impl AudioCommands for Pactl {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        run_pactl(args)
    }
}

fn run_pactl(args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("pactl").args(args).env("LC_ALL", "C")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|error| format!("Audio controls require pactl and a running PipeWire-Pulse or PulseAudio server: {error}"))?;
    let stdout = child.stdout.take().ok_or("Missing audio command output.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Missing audio command diagnostics.")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(16_385).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                let _ = errors.join();
                return Err("The audio server did not respond within two seconds.".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "Audio reader stopped.")?
        .map_err(|error| error.to_string())?;
    let diagnostics = errors
        .join()
        .map_err(|_| "Audio diagnostics stopped.")?
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!(
            "Audio server rejected the request: {}",
            String::from_utf8_lossy(&diagnostics).trim()
        ));
    }
    if bytes.len() as u64 > MAX_OUTPUT {
        return Err("The audio device list is too large.".into());
    }
    String::from_utf8(bytes).map_err(|_| "The audio server returned invalid text.".into())
}

fn devices(
    source: &str,
    kind: DeviceKind,
    default_name: Option<&str>,
) -> Result<Vec<AudioDevice>, String> {
    let list: Vec<Value> = serde_json::from_str(source)
        .map_err(|error| format!("Unreadable audio device list: {error}"))?;
    if list.len() > 256 {
        return Err("Too many audio devices were reported.".into());
    }
    let mut devices = Vec::new();
    for device in list {
        let Some(name) = device["name"].as_str().filter(|name| !name.is_empty()) else {
            continue;
        };
        if kind == DeviceKind::Input
            && (name.ends_with(".monitor")
                || device["monitor_of_sink"]
                    .as_u64()
                    .is_some_and(|index| index != u32::MAX as u64))
        {
            continue;
        }
        let index = device["index"]
            .as_u64()
            .and_then(|index| u32::try_from(index).ok())
            .ok_or("An audio device has an invalid index.")?;
        let channels = device["volume"]
            .as_object()
            .ok_or("An audio device has no readable volume.")?;
        let volume = channels
            .values()
            .filter_map(|channel| channel["value"].as_u64())
            .max()
            .ok_or("An audio device has no channel volume.")?;
        let muted = device["mute"]
            .as_bool()
            .ok_or("An audio device has no readable mute state.")?;
        let port_name = device["active_port"].as_str();
        let port = port_name
            .and_then(|name| {
                device["ports"]
                    .as_array()?
                    .iter()
                    .find(|port| port["name"].as_str() == Some(name))
            })
            .and_then(|port| port["description"].as_str())
            .or(port_name)
            .map(str::to_string);
        let label = device["description"]
            .as_str()
            .filter(|label| !label.is_empty())
            .unwrap_or(name);
        let prefix = if kind == DeviceKind::Output {
            "output"
        } else {
            "input"
        };
        devices.push(AudioDevice {
            id: format!("{prefix}:{index}:{name}"),
            index,
            name: name.into(),
            label: label.into(),
            kind,
            volume_percent: volume as f64 / 65536.0 * 100.0,
            muted,
            active_port: port,
            is_default: Some(name) == default_name,
        });
    }
    devices.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(devices)
}

pub fn snapshot() -> Result<AudioSnapshot, String> {
    snapshot_with(&Pactl)
}

pub fn snapshot_with(commands: &impl AudioCommands) -> Result<AudioSnapshot, String> {
    let info: Value = serde_json::from_str(&commands.run(&["--format=json", "info"])?)
        .map_err(|error| format!("Unreadable audio server information: {error}"))?;
    Ok(AudioSnapshot {
        server: info["server_name"]
            .as_str()
            .unwrap_or("Session audio server")
            .into(),
        outputs: devices(
            &commands.run(&["--format=json", "list", "sinks"])?,
            DeviceKind::Output,
            info["default_sink_name"].as_str(),
        )?,
        inputs: devices(
            &commands.run(&["--format=json", "list", "sources"])?,
            DeviceKind::Input,
            info["default_source_name"].as_str(),
        )?,
    })
}

pub fn resolve_device<'a>(
    snapshot: &'a AudioSnapshot,
    id: &str,
) -> Result<&'a AudioDevice, String> {
    snapshot
        .outputs
        .iter()
        .chain(&snapshot.inputs)
        .find(|device| device.id == id)
        .ok_or_else(|| "That audio device is no longer available. Refresh the device list.".into())
}

pub fn apply(action: AudioAction) -> Result<AudioSnapshot, String> {
    apply_with(&Pactl, action)
}

pub fn apply_with(
    commands: &impl AudioCommands,
    action: AudioAction,
) -> Result<AudioSnapshot, String> {
    if let AudioAction::SetVolume { percent, .. } = &action
        && (!percent.is_finite() || !(0.0..=150.0).contains(percent))
    {
        return Err("Volume must be between 0% and 150%.".into());
    }
    let current = snapshot_with(commands)?;
    let id = match &action {
        AudioAction::SetDefault { device_id }
        | AudioAction::SetVolume { device_id, .. }
        | AudioAction::SetMute { device_id, .. } => device_id,
    };
    let device = resolve_device(&current, id)?;
    if let AudioAction::SetVolume { percent, .. } = &action
        && device.kind == DeviceKind::Input
        && *percent > 100.0
    {
        return Err("Microphone volume must be between 0% and 100%.".into());
    }
    let index = device.index.to_string();
    let command = match (&action, device.kind) {
        (AudioAction::SetDefault { .. }, DeviceKind::Output) => "set-default-sink",
        (AudioAction::SetDefault { .. }, DeviceKind::Input) => "set-default-source",
        (AudioAction::SetVolume { .. }, DeviceKind::Output) => "set-sink-volume",
        (AudioAction::SetVolume { .. }, DeviceKind::Input) => "set-source-volume",
        (AudioAction::SetMute { .. }, DeviceKind::Output) => "set-sink-mute",
        (AudioAction::SetMute { .. }, DeviceKind::Input) => "set-source-mute",
    };
    match action {
        AudioAction::SetDefault { .. } => {
            if device.name.starts_with('-') {
                return Err("This device has an unsupported server name.".into());
            }
            commands.run(&[command, &device.name])?;
        }
        AudioAction::SetVolume { percent, .. } => {
            commands.run(&[command, &index, &format!("{percent:.2}%")])?;
        }
        AudioAction::SetMute { muted, .. } => {
            commands.run(&[command, &index, if muted { "1" } else { "0" }])?;
        }
    }
    snapshot_with(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Fixture {
        calls: RefCell<Vec<Vec<String>>>,
        fail: bool,
    }
    impl AudioCommands for Fixture {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).to_string()).collect());
            match args {
                ["--format=json", "info"] => Ok(r#"{"server_name":"PulseAudio (on PipeWire)","default_sink_name":"studio.output","default_source_name":"usb.mic"}"#.into()),
                ["--format=json", "list", kind] => Ok(serde_json::to_string(&if *kind == "sinks" {
                    vec![serde_json::json!({"index":7,"name":"studio.output","description":"Studio speakers","mute":false,"volume":{"left":{"value":32768},"right":{"value":16384}},"active_port":"speaker","ports":[{"name":"speaker","description":"Speakers"}]})]
                } else {
                    vec![serde_json::json!({"index":3,"name":"usb.mic","description":"USB microphone","mute":true,"monitor_of_sink":4294967295_u64,"volume":{"mono":{"value":65536}}}),
                         serde_json::json!({"index":4,"name":"studio.output.monitor"}),
                         serde_json::json!({"index":5,"name":"loopback","monitor_of_sink":7})]
                }).unwrap()),
                _ if self.fail => Err("fixture rejected control".into()),
                _ => Ok(String::new()),
            }
        }
    }
    fn fixture(fail: bool) -> Fixture {
        Fixture {
            calls: RefCell::new(Vec::new()),
            fail,
        }
    }
    #[test]
    fn reads_defaults_channel_volume_ports_and_filters_monitor_sources() {
        let snapshot = snapshot_with(&fixture(false)).unwrap();
        assert_eq!(snapshot.inputs.len(), 1);
        assert_eq!(snapshot.outputs[0].volume_percent, 50.0);
        assert_eq!(snapshot.outputs[0].active_port.as_deref(), Some("Speakers"));
        assert!(snapshot.outputs[0].is_default);
        assert!(snapshot.inputs[0].is_default && snapshot.inputs[0].muted);
    }
    #[test]
    fn validates_levels_and_current_identity_before_executing_fixed_commands() {
        for percent in [f64::NAN, f64::INFINITY, -1.0, 151.0] {
            let fixture = fixture(false);
            assert!(
                apply_with(
                    &fixture,
                    AudioAction::SetVolume {
                        device_id: "output:7:studio.output".into(),
                        percent
                    }
                )
                .is_err()
            );
            assert!(fixture.calls.borrow().is_empty());
        }
        let backend = fixture(false);
        assert!(
            apply_with(
                &backend,
                AudioAction::SetMute {
                    device_id: "output:7:old-device".into(),
                    muted: true
                }
            )
            .is_err()
        );
        assert!(
            !backend
                .calls
                .borrow()
                .iter()
                .any(|args| args[0].starts_with("set-"))
        );
        apply_with(
            &backend,
            AudioAction::SetVolume {
                device_id: "output:7:studio.output".into(),
                percent: 32.5,
            },
        )
        .unwrap();
        assert!(backend.calls.borrow().contains(&vec![
            "set-sink-volume".into(),
            "7".into(),
            "32.50%".into()
        ]));
        apply_with(
            &backend,
            AudioAction::SetDefault {
                device_id: "input:3:usb.mic".into(),
            },
        )
        .unwrap();
        assert!(
            backend
                .calls
                .borrow()
                .contains(&vec!["set-default-source".into(), "usb.mic".into()])
        );
        apply_with(
            &backend,
            AudioAction::SetMute {
                device_id: "input:3:usb.mic".into(),
                muted: false,
            },
        )
        .unwrap();
        assert!(backend.calls.borrow().contains(&vec![
            "set-source-mute".into(),
            "3".into(),
            "0".into()
        ]));
    }
    #[test]
    fn output_boost_preserves_gtk_range_but_microphones_remain_bounded() {
        let backend = fixture(false);
        apply_with(
            &backend,
            AudioAction::SetVolume {
                device_id: "output:7:studio.output".into(),
                percent: 150.0,
            },
        )
        .unwrap();
        assert!(backend.calls.borrow().contains(&vec![
            "set-sink-volume".into(),
            "7".into(),
            "150.00%".into()
        ]));
        let backend = fixture(false);
        assert!(
            apply_with(
                &backend,
                AudioAction::SetVolume {
                    device_id: "input:3:usb.mic".into(),
                    percent: 100.1
                }
            )
            .is_err()
        );
        assert!(
            !backend
                .calls
                .borrow()
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg.starts_with("set-")))
        );
    }
    #[test]
    fn reports_rejection_and_malformed_server_data_instead_of_success() {
        let result = apply_with(
            &fixture(true),
            AudioAction::SetMute {
                device_id: "output:7:studio.output".into(),
                muted: true,
            },
        );
        assert_eq!(result.unwrap_err(), "fixture rejected control");
        assert!(devices("not json", DeviceKind::Input, None).is_err());
        assert!(
            devices(
                r#"[{"name":"mic","index":1,"mute":false}]"#,
                DeviceKind::Input,
                None
            )
            .is_err()
        );
        assert!(serde_json::from_value::<AudioAction>(serde_json::json!({"type":"set_mute","device_id":"anything","muted":true,"command":"not allowed"})).is_err());
    }
}
