use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
}

impl PowerCommand {
    fn systemctl_arg(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Hibernate => "hibernate",
            Self::Reboot => "reboot",
            Self::PowerOff => "poweroff",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryStatus {
    pub name: String,
    pub percentage: Option<u8>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSnapshot {
    pub batteries: Vec<BatteryStatus>,
    pub line_power_online: Option<bool>,
    pub performance_profile: Option<String>,
}

impl PowerSnapshot {
    pub fn has_battery(&self) -> bool {
        !self.batteries.is_empty()
    }
}

#[derive(Debug)]
pub enum PowerError {
    Io(io::Error),
    CommandFailed {
        program: &'static str,
        args: Vec<String>,
        status: ExitStatus,
    },
}

impl fmt::Display for PowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::CommandFailed {
                program,
                args,
                status,
            } => {
                write!(f, "{program} {} exited with {status}", args.join(" "))
            }
        }
    }
}

impl std::error::Error for PowerError {}

impl From<io::Error> for PowerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
pub struct PowerManager {
    power_supply_path: PathBuf,
}

impl Default for PowerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerManager {
    pub fn new() -> Self {
        Self {
            power_supply_path: PathBuf::from("/sys/class/power_supply"),
        }
    }

    pub fn with_power_supply_path(path: impl Into<PathBuf>) -> Self {
        Self {
            power_supply_path: path.into(),
        }
    }

    pub fn execute(&self, command: PowerCommand) -> Result<(), PowerError> {
        run_status("systemctl", &["--no-block", command.systemctl_arg()])
    }

    pub fn suspend(&self) -> Result<(), PowerError> {
        self.execute(PowerCommand::Suspend)
    }

    pub fn hibernate(&self) -> Result<(), PowerError> {
        self.execute(PowerCommand::Hibernate)
    }

    pub fn reboot(&self) -> Result<(), PowerError> {
        self.execute(PowerCommand::Reboot)
    }

    pub fn power_off(&self) -> Result<(), PowerError> {
        self.execute(PowerCommand::PowerOff)
    }

    pub fn set_performance_profile(&self, profile: &str) -> Result<(), PowerError> {
        run_status("powerprofilesctl", &["set", profile])
    }

    pub fn snapshot(&self) -> PowerSnapshot {
        PowerSnapshot {
            batteries: self.batteries(),
            line_power_online: self.line_power_online(),
            performance_profile: current_performance_profile(),
        }
    }

    fn batteries(&self) -> Vec<BatteryStatus> {
        let Ok(entries) = fs::read_dir(&self.power_supply_path) else {
            return Vec::new();
        };

        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let supply_type = read_trimmed(path.join("type"))?;
                if supply_type != "Battery" {
                    return None;
                }

                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Battery")
                    .to_string();
                let percentage =
                    read_trimmed(path.join("capacity")).and_then(|value| value.parse::<u8>().ok());
                let state = read_trimmed(path.join("status"));

                Some(BatteryStatus {
                    name,
                    percentage,
                    state,
                })
            })
            .collect()
    }

    fn line_power_online(&self) -> Option<bool> {
        let entries = fs::read_dir(&self.power_supply_path).ok()?;

        for entry in entries.flatten() {
            let path = entry.path();
            let supply_type = read_trimmed(path.join("type"));
            if !matches!(supply_type.as_deref(), Some("Mains") | Some("USB")) {
                continue;
            }

            if let Some(online) = read_trimmed(path.join("online")) {
                return Some(online == "1");
            }
        }

        None
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_performance_profile() -> Option<String> {
    let output = Command::new("powerprofilesctl").arg("get").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_status(program: &'static str, args: &[&str]) -> Result<(), PowerError> {
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        return Ok(());
    }

    Err(PowerError::CommandFailed {
        program,
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        status,
    })
}

pub fn command_timeout() -> Duration {
    Duration::from_secs(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_rejected_command() {
        let err = run_status("sh", &["-c", "exit 7"]).unwrap_err();
        assert!(matches!(err, PowerError::CommandFailed { .. }));
    }

    #[test]
    fn reads_battery_snapshot_from_sysfs_shape() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-power-test-{}", std::process::id()));
        let battery = root.join("BAT0");
        let mains = root.join("AC");
        fs::create_dir_all(&battery).unwrap();
        fs::create_dir_all(&mains).unwrap();
        fs::write(battery.join("type"), "Battery\n").unwrap();
        fs::write(battery.join("capacity"), "73\n").unwrap();
        fs::write(battery.join("status"), "Discharging\n").unwrap();
        fs::write(mains.join("type"), "Mains\n").unwrap();
        fs::write(mains.join("online"), "0\n").unwrap();

        let manager = PowerManager::with_power_supply_path(&root);
        let snapshot = manager.snapshot();

        assert_eq!(snapshot.batteries.len(), 1);
        assert_eq!(snapshot.batteries[0].name, "BAT0");
        assert_eq!(snapshot.batteries[0].percentage, Some(73));
        assert_eq!(snapshot.batteries[0].state.as_deref(), Some("Discharging"));
        assert_eq!(snapshot.line_power_online, Some(false));

        let _ = fs::remove_dir_all(root);
    }
}
