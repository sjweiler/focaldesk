use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutomationConfig {
    #[serde(default, rename = "automation")]
    pub automations: Vec<AutomationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationEntry {
    pub name: String,
    /// Path to a `.lua` script. Relative paths resolve against
    /// [`scripts_dir`]; absolute paths are used as-is.
    pub script: String,
    /// See [`crate::schedule::Schedule::parse`] for accepted syntax.
    pub schedule: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

fn automation_root() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("automation")
}

pub fn config_path() -> PathBuf {
    automation_root().join("automations.toml")
}

pub fn scripts_dir() -> PathBuf {
    automation_root().join("scripts")
}

pub fn script_path(script: &str) -> PathBuf {
    let path = Path::new(script);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        scripts_dir().join(path)
    }
}

/// Missing or unparsable config is treated as "no automations configured"
/// rather than a startup failure — the daemon should still come up and
/// serve `Status`/`ListAutomations` over IPC.
pub fn load_config() -> AutomationConfig {
    let path = config_path();

    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(
                target: "focaldesk.automation",
                path = %path.display(),
                error = %err,
                "failed to parse automations.toml, starting with zero automations"
            );
            AutomationConfig::default()
        }),
        Err(_) => AutomationConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_path_resolves_relative_against_scripts_dir() {
        let resolved = script_path("reminder.lua");
        assert_eq!(resolved, scripts_dir().join("reminder.lua"));
    }

    #[test]
    fn script_path_leaves_absolute_paths_untouched() {
        let resolved = script_path("/opt/scripts/reminder.lua");
        assert_eq!(resolved, PathBuf::from("/opt/scripts/reminder.lua"));
    }

    #[test]
    fn parses_automations_toml() {
        let toml = r#"
            [[automation]]
            name = "standup-reminder"
            script = "standup.lua"
            schedule = "daily 09:00"

            [[automation]]
            name = "disabled-check"
            script = "check.lua"
            schedule = "every 5m"
            enabled = false
        "#;

        let config: AutomationConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.automations.len(), 2);
        assert!(config.automations[0].enabled);
        assert!(!config.automations[1].enabled);
    }
}
