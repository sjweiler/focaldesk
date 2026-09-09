use std::collections::HashMap;

use super::run_control_command;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AudioDeviceKind {
    Sink,
    Source,
}

#[derive(Debug, Default)]
pub(super) struct PactlAudioDevice {
    name: Option<String>,
    description: Option<String>,
    active_port: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct AudioDeviceChoice {
    pub(super) selector: String,
    pub(super) label: String,
}

pub(super) fn normalize_audio_label(label: &str) -> String {
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

pub(super) fn audio_label_key(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

pub(super) fn push_unique_audio_device(
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

pub(super) fn pactl_value(line: &str, key: &str) -> Option<String> {
    line.trim()
        .strip_prefix(key)?
        .trim()
        .trim_matches('"')
        .trim()
        .to_string()
        .into()
}

pub(super) fn pactl_port_label(line: &str, port_name: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix(port_name)?.strip_prefix(':')?.trim();
    let label = rest.split(" (").next().unwrap_or(rest).trim();
    (!label.is_empty()).then(|| label.to_string())
}

pub(super) fn pactl_device_label(
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

pub(super) fn push_pactl_device(
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

pub(super) fn parse_pactl_devices(
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

pub(super) fn parse_pactl_short_devices(
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

pub(super) fn parse_wpctl_devices(output: &str, kind: AudioDeviceKind) -> Vec<AudioDeviceChoice> {
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

pub(super) fn parse_pactl_default_device(output: &str, kind: AudioDeviceKind) -> Option<String> {
    let key = match kind {
        AudioDeviceKind::Sink => "Default Sink:",
        AudioDeviceKind::Source => "Default Source:",
    };

    output.lines().find_map(|line| pactl_value(line, key))
}

pub(super) fn load_audio_devices(kind: AudioDeviceKind) -> Result<Vec<AudioDeviceChoice>, String> {
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

pub(super) fn set_default_audio_device(
    kind: AudioDeviceKind,
    selector: &str,
) -> Result<(), String> {
    if let Some(id) = selector.strip_prefix("wpctl:") {
        return run_control_command("wpctl", &["set-default", id]).map(|_| ());
    }

    let command = match kind {
        AudioDeviceKind::Sink => "set-default-sink",
        AudioDeviceKind::Source => "set-default-source",
    };
    run_control_command("pactl", &[command, selector]).map(|_| ())
}

pub(super) fn parse_wpctl_volume(output: &str) -> Option<f64> {
    output
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
        .map(|volume| (volume * 100.0).clamp(0.0, 150.0))
}

pub(super) fn parse_pactl_volume(output: &str) -> Option<f64> {
    output.split_whitespace().find_map(|part| {
        part.strip_suffix('%')
            .and_then(|percent| percent.parse::<f64>().ok())
    })
}

pub(super) fn load_default_output_volume() -> Result<f64, String> {
    match run_control_command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        Ok(output) => parse_wpctl_volume(&output)
            .ok_or_else(|| "wpctl returned an unreadable volume".to_string()),
        Err(wpctl_err) => {
            let output = run_control_command("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])
                .map_err(|pactl_err| format!("{wpctl_err}; {pactl_err}"))?;
            parse_pactl_volume(&output)
                .ok_or_else(|| "pactl returned an unreadable volume".to_string())
        }
    }
}

pub(super) fn set_default_output_volume(percent: f64) -> Result<(), String> {
    let percent = percent.clamp(0.0, 150.0);
    let wpctl_value = format!("{:.3}", percent / 100.0);
    match run_control_command(
        "wpctl",
        &["set-volume", "@DEFAULT_AUDIO_SINK@", &wpctl_value],
    ) {
        Ok(_) => Ok(()),
        Err(wpctl_err) => {
            let pactl_value = format!("{percent:.0}%");
            run_control_command(
                "pactl",
                &["set-sink-volume", "@DEFAULT_SINK@", &pactl_value],
            )
            .map(|_| ())
            .map_err(|pactl_err| format!("{wpctl_err}; {pactl_err}"))
        }
    }
}
