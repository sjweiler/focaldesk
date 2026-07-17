use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
}

impl PowerCommand {
    fn login1_method(self) -> &'static str {
        match self {
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
            Self::Reboot => "Reboot",
            Self::PowerOff => "PowerOff",
        }
    }

    fn login1_capability_method(self) -> &'static str {
        match self {
            Self::Suspend => "CanSuspend",
            Self::Hibernate => "CanHibernate",
            Self::Reboot => "CanReboot",
            Self::PowerOff => "CanPowerOff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAuthorization {
    Allowed,
    Challenge,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub name: String,
    pub percentage: Option<u8>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerSnapshot {
    pub batteries: Vec<BatteryStatus>,
    pub line_power_online: Option<bool>,
    pub performance_profile: Option<String>,
    pub captured_at_unix_ms: u64,
}

pub const LOW_BATTERY_THRESHOLD_PERCENT: u8 = 15;

impl PowerSnapshot {
    pub fn has_battery(&self) -> bool {
        !self.batteries.is_empty()
    }

    pub fn lowest_battery_percentage(&self) -> Option<u8> {
        self.batteries
            .iter()
            .filter_map(|battery| battery.percentage)
            .min()
    }

    pub fn is_low_battery(&self, threshold: u8) -> bool {
        self.lowest_battery_percentage()
            .is_some_and(|percentage| percentage <= threshold)
    }

    pub fn is_charging(&self) -> bool {
        self.batteries.iter().any(|battery| {
            battery
                .state
                .as_deref()
                .is_some_and(|state| state.eq_ignore_ascii_case("charging"))
        })
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
    InvalidResponse(String),
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
            Self::InvalidResponse(response) => {
                write!(f, "unexpected login1 capability response: {response}")
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
        self.execute_with_interactive_authorization(command, true)
    }

    /// Execute a policy-driven action without allowing PolicyKit to start an
    /// authentication conversation. This is required for unattended actions
    /// such as idle suspend, where no user may be available to answer a prompt.
    pub fn execute_noninteractive(&self, command: PowerCommand) -> Result<(), PowerError> {
        self.execute_with_interactive_authorization(command, false)
    }

    fn execute_with_interactive_authorization(
        &self,
        command: PowerCommand,
        allow_interactive: bool,
    ) -> Result<(), PowerError> {
        let authorization_option = if allow_interactive {
            "--allow-interactive-authorization=yes"
        } else {
            "--allow-interactive-authorization=no"
        };
        let interactive_argument = if allow_interactive { "true" } else { "false" };
        run_status(
            "busctl",
            &[
                "--system",
                authorization_option,
                "--timeout=120",
                "call",
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                command.login1_method(),
                "b",
                interactive_argument,
            ],
        )
    }

    /// Report whether logind will allow this session's action directly or
    /// require an interactive PolicyKit challenge.
    pub fn authorization(&self, command: PowerCommand) -> Result<PowerAuthorization, PowerError> {
        let args = [
            "--system",
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            command.login1_capability_method(),
        ];
        let output = Command::new("busctl").args(args).output()?;
        if !output.status.success() {
            return Err(PowerError::CommandFailed {
                program: "busctl",
                args: args.iter().map(|value| (*value).to_string()).collect(),
                status: output.status,
            });
        }

        parse_login1_capability(&String::from_utf8_lossy(&output.stdout))
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
            captured_at_unix_ms: current_unix_time_ms(),
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

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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

fn parse_login1_capability(response: &str) -> Result<PowerAuthorization, PowerError> {
    let value = response
        .trim()
        .strip_prefix("s ")
        .unwrap_or(response.trim());
    match value.trim_matches('"') {
        "yes" => Ok(PowerAuthorization::Allowed),
        "challenge" => Ok(PowerAuthorization::Challenge),
        "no" | "na" => Ok(PowerAuthorization::Unavailable),
        _ => Err(PowerError::InvalidResponse(response.trim().to_string())),
    }
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
    fn maps_power_commands_to_login1_methods() {
        assert_eq!(PowerCommand::Suspend.login1_method(), "Suspend");
        assert_eq!(PowerCommand::Hibernate.login1_method(), "Hibernate");
        assert_eq!(PowerCommand::Reboot.login1_method(), "Reboot");
        assert_eq!(PowerCommand::PowerOff.login1_method(), "PowerOff");
    }

    #[test]
    fn parses_login1_capability_responses() {
        assert_eq!(
            parse_login1_capability("s \"yes\"\n").unwrap(),
            PowerAuthorization::Allowed
        );
        assert_eq!(
            parse_login1_capability("s \"challenge\"\n").unwrap(),
            PowerAuthorization::Challenge
        );
        assert_eq!(
            parse_login1_capability("s \"no\"\n").unwrap(),
            PowerAuthorization::Unavailable
        );
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

    #[test]
    fn detects_low_battery_and_charging() {
        let snapshot = PowerSnapshot {
            batteries: vec![
                BatteryStatus {
                    name: "BAT0".to_string(),
                    percentage: Some(9),
                    state: Some("Discharging".to_string()),
                },
                BatteryStatus {
                    name: "BAT1".to_string(),
                    percentage: Some(33),
                    state: Some("Charging".to_string()),
                },
            ],
            line_power_online: Some(false),
            performance_profile: None,
            captured_at_unix_ms: 0,
        };

        assert_eq!(snapshot.lowest_battery_percentage(), Some(9));
        assert!(snapshot.is_low_battery(LOW_BATTERY_THRESHOLD_PERCENT));
        assert!(snapshot.is_charging());
    }
}
