use std::process::Command;

/// Return whether the current audio server exposes a real input source.
///
/// PulseAudio/PipeWire monitor sources mirror speaker output and are not
/// microphones, so they are deliberately excluded.
pub fn microphone_detected() -> bool {
    pactl_sources_detect_microphone(&command_stdout("pactl", &["list", "short", "sources"]))
        || wpctl_status_detects_microphone(&command_stdout("wpctl", &["status"]))
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

fn pactl_sources_detect_microphone(output: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split('\t');
        let _index = fields.next();
        fields
            .next()
            .map(str::trim)
            .is_some_and(|name| !name.is_empty() && !name.ends_with(".monitor"))
    })
}

fn wpctl_status_detects_microphone(output: &str) -> bool {
    let mut in_sources = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Sources:") {
            in_sources = true;
            continue;
        }
        if !in_sources {
            continue;
        }
        if trimmed.ends_with(':') {
            break;
        }

        let Some((_, label)) = trimmed.split_once(". ") else {
            continue;
        };
        let label = label.to_ascii_lowercase();
        if !label.trim().is_empty() && !label.contains("monitor") {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{pactl_sources_detect_microphone, wpctl_status_detects_microphone};

    #[test]
    fn pactl_ignores_monitor_sources() {
        assert!(!pactl_sources_detect_microphone(
            "42\talsa_output.pci.stereo.monitor\tPipeWire\n"
        ));
        assert!(pactl_sources_detect_microphone(
            "42\talsa_output.pci.stereo.monitor\tPipeWire\n43\talsa_input.usb.mono\tPipeWire\n"
        ));
    }

    #[test]
    fn wpctl_detects_devices_in_sources_section_only() {
        let status = "Audio\n ├─ Sinks:\n │  * 42. Speakers [vol: 1.00]\n ├─ Sources:\n │  * 43. USB Microphone [vol: 1.00]\n └─ Streams:\n";
        assert!(wpctl_status_detects_microphone(status));

        let monitor_only =
            "Audio\n ├─ Sources:\n │  * 42. Monitor of Speakers [vol: 1.00]\n └─ Streams:\n";
        assert!(!wpctl_status_detects_microphone(monitor_only));
    }
}
