use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const AGENT_MANIFEST_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTriggerKind {
    Schedule,
    DesktopEvent,
    VoicePhrase,
    Hotkey,
    IpcEvent,
}

impl AgentTriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::DesktopEvent => "desktop_event",
            Self::VoicePhrase => "voice_phrase",
            Self::Hotkey => "hotkey",
            Self::IpcEvent => "ipc_event",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTrigger {
    pub id: String,
    pub kind: AgentTriggerKind,
    pub objective: String,
    #[serde(default)]
    pub match_value: String,
    #[serde(default)]
    pub interval_seconds: Option<u64>,
    #[serde(default = "default_trigger_cooldown")]
    pub cooldown_seconds: u64,
    #[serde(default = "default_trigger_hourly_limit")]
    pub max_runs_per_hour: usize,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub manifest_version: u16,
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tool_allowlist: Vec<String>,
    #[serde(default = "default_max_tool_steps")]
    pub max_tool_steps: usize,
    #[serde(default = "default_max_context_chars")]
    pub max_context_chars: usize,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub memory: bool,
    #[serde(default)]
    pub voice: bool,
    #[serde(default)]
    pub triggers: Vec<AgentTrigger>,
    #[serde(default)]
    pub daily_token_limit: Option<u64>,
    #[serde(default)]
    pub daily_cost_limit_microusd: Option<u64>,
    #[serde(default)]
    pub input_cost_microusd_per_million: Option<u64>,
    #[serde(default)]
    pub output_cost_microusd_per_million: Option<u64>,
    #[serde(default)]
    pub capability_policy: Option<crate::CapabilityPolicy>,
    #[serde(default)]
    pub built_in: bool,
}

const fn default_max_tool_steps() -> usize {
    4
}

const fn default_max_context_chars() -> usize {
    48_000
}

const fn default_max_output_tokens() -> u32 {
    1_024
}

const fn default_timeout_seconds() -> u64 {
    120
}

const fn default_trigger_cooldown() -> u64 {
    60
}

const fn default_trigger_hourly_limit() -> usize {
    6
}

const fn default_true() -> bool {
    true
}

impl AgentDefinition {
    pub fn from_toml(input: &str) -> Result<Self> {
        let definition: Self = toml::from_str(input)?;
        definition.validate()?;
        Ok(definition)
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != AGENT_MANIFEST_VERSION {
            bail!(
                "unsupported agent manifest version {}; supported version is {}",
                self.manifest_version,
                AGENT_MANIFEST_VERSION
            );
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            bail!("agent id must contain 1-64 lowercase ASCII letters, digits, '-' or '_'");
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            bail!("agent name must contain 1-80 characters");
        }
        if self.description.chars().count() > 500 || self.instructions.chars().count() > 4_000 {
            bail!("agent description or instructions exceed the manifest limit");
        }
        if !(1..=crate::planner::MAX_AGENT_STEPS).contains(&self.max_tool_steps) {
            bail!("agent max_tool_steps must be between 1 and 4");
        }
        if !(8_000..=128_000).contains(&self.max_context_chars) {
            bail!("agent max_context_chars must be between 8000 and 128000");
        }
        if !(128..=4_096).contains(&self.max_output_tokens) {
            bail!("agent max_output_tokens must be between 128 and 4096");
        }
        if !(10..=120).contains(&self.timeout_seconds) {
            bail!("agent timeout_seconds must be between 10 and 120");
        }
        if self.tool_allowlist.is_empty() || self.tool_allowlist.len() > 64 {
            bail!("agent tool_allowlist must contain 1-64 tools");
        }
        let unique = self.tool_allowlist.iter().collect::<BTreeSet<_>>();
        if unique.len() != self.tool_allowlist.len()
            || self.tool_allowlist.iter().any(|tool| {
                tool.is_empty()
                    || tool.len() > 80
                    || !tool
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        {
            bail!("agent tool_allowlist contains an invalid or duplicate tool name");
        }
        if self.triggers.len() > 16 {
            bail!("agent manifest may define at most 16 triggers");
        }
        let trigger_ids = self
            .triggers
            .iter()
            .map(|trigger| &trigger.id)
            .collect::<BTreeSet<_>>();
        if trigger_ids.len() != self.triggers.len() {
            bail!("agent trigger ids must be unique");
        }
        for trigger in &self.triggers {
            trigger.validate(self.voice)?;
        }
        if self.daily_token_limit.is_some_and(|limit| limit < 1_000) {
            bail!("agent daily_token_limit must be at least 1000 when configured");
        }
        if self.daily_cost_limit_microusd.is_some()
            && (self.input_cost_microusd_per_million.is_none()
                || self.output_cost_microusd_per_million.is_none())
        {
            bail!("agent daily cost limit requires input and output pricing");
        }
        if let Some(policy) = &self.capability_policy {
            policy.validate()?;
        }
        Ok(())
    }
}

impl AgentTrigger {
    pub fn validate(&self, voice_capable: bool) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            bail!("agent trigger id must contain 1-64 lowercase ASCII letters, digits, '-' or '_'");
        }
        if self.objective.trim().is_empty() || self.objective.chars().count() > 4_000 {
            bail!("agent trigger objective must contain 1-4000 characters");
        }
        if self.match_value.chars().count() > 200 {
            bail!("agent trigger match_value exceeds 200 characters");
        }
        if self.cooldown_seconds > 86_400 {
            bail!("agent trigger cooldown_seconds must not exceed 86400");
        }
        if !(1..=60).contains(&self.max_runs_per_hour) {
            bail!("agent trigger max_runs_per_hour must be between 1 and 60");
        }
        match self.kind {
            AgentTriggerKind::Schedule => {
                if !self.match_value.is_empty() {
                    bail!("schedule trigger match_value must be empty");
                }
                if !matches!(self.interval_seconds, Some(60..=86_400)) {
                    bail!("schedule trigger interval_seconds must be between 60 and 86400");
                }
            }
            AgentTriggerKind::VoicePhrase => {
                if !voice_capable {
                    bail!("voice phrase trigger requires voice=true");
                }
                if self.match_value.trim().is_empty() || self.interval_seconds.is_some() {
                    bail!("voice phrase trigger requires match_value and no interval");
                }
            }
            _ => {
                if self.match_value.trim().is_empty() || self.interval_seconds.is_some() {
                    bail!("event trigger requires match_value and no interval");
                }
            }
        }
        Ok(())
    }
}

pub fn built_in_agents() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            manifest_version: AGENT_MANIFEST_VERSION,
            id: "desktop".into(),
            name: "Desktop Assistant".into(),
            description: "Inspects FocalDesk and proposes explicitly confirmed desktop actions."
                .into(),
            instructions: "Prefer the smallest set of desktop inspections needed. Never claim that a proposed action already happened.".into(),
            tool_allowlist: vec![
                "get_session_status", "list_outputs", "get_output_details", "list_windows",
                "list_workspaces", "get_service_health", "get_rendering_status",
                "show_notification", "focus_window", "move_window_to_workspace",
                "open_settings_panel",
            ].into_iter().map(str::to_string).collect(),
            max_tool_steps: 4,
            max_context_chars: default_max_context_chars(),
            max_output_tokens: default_max_output_tokens(),
            timeout_seconds: default_timeout_seconds(),
            memory: false,
            voice: true,
            triggers: Vec::new(),
            daily_token_limit: None,
            daily_cost_limit_microusd: None,
            input_cost_microusd_per_million: None,
            output_cost_microusd_per_million: None,
            capability_policy: None,
            built_in: true,
        },
        AgentDefinition {
            manifest_version: AGENT_MANIFEST_VERSION,
            id: "troubleshooter".into(),
            name: "System Troubleshooter".into(),
            description: "Diagnoses session, service, display, and rendering problems without changing state.".into(),
            instructions: "Use diagnostic evidence only. Clearly distinguish observed failures from hypotheses.".into(),
            tool_allowlist: vec![
                "get_session_status", "list_outputs", "get_output_details", "get_service_health",
                "search_recent_logs", "get_rendering_status",
            ].into_iter().map(str::to_string).collect(),
            max_tool_steps: 4,
            max_context_chars: default_max_context_chars(),
            max_output_tokens: default_max_output_tokens(),
            timeout_seconds: default_timeout_seconds(),
            memory: false,
            voice: false,
            triggers: Vec::new(),
            daily_token_limit: None,
            daily_cost_limit_microusd: None,
            input_cost_microusd_per_million: None,
            output_cost_microusd_per_million: None,
            capability_policy: None,
            built_in: true,
        },
        AgentDefinition {
            manifest_version: AGENT_MANIFEST_VERSION,
            id: "accessibility".into(),
            name: "Accessibility Assistant".into(),
            description: "Provides voice-friendly desktop inspection and navigation assistance.".into(),
            instructions: "Keep responses brief and suitable for speech. Describe the exact effect of every proposed action.".into(),
            tool_allowlist: vec![
                "get_session_status", "list_windows", "list_workspaces", "show_notification",
                "focus_window", "move_window_to_workspace", "open_settings_panel",
            ].into_iter().map(str::to_string).collect(),
            max_tool_steps: 3,
            max_context_chars: 32_000,
            max_output_tokens: 512,
            timeout_seconds: 90,
            memory: false,
            voice: true,
            triggers: Vec::new(),
            daily_token_limit: None,
            daily_cost_limit_microusd: None,
            input_cost_microusd_per_million: None,
            output_cost_microusd_per_million: None,
            capability_policy: None,
            built_in: true,
        },
    ]
}

/// Loads bounded declarative manifests. Manifests can narrow model behavior
/// and tool visibility, but cannot add executable code or mark themselves as
/// built in.
pub fn load_agent_definitions(directory: &Path) -> Result<Vec<AgentDefinition>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("agent manifest path must be a real directory");
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()? {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "agent manifest directory must not contain symlinks: {}",
                path.display()
            );
        }
        if metadata.is_file() && path.extension().and_then(|value| value.to_str()) == Some("toml") {
            paths.push(path);
        } else if metadata.is_dir() {
            let package_manifest = path.join("agent.toml");
            if package_manifest.exists() {
                let manifest_metadata = std::fs::symlink_metadata(&package_manifest)?;
                if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
                    bail!(
                        "agent package manifest must be a regular file: {}",
                        package_manifest.display()
                    );
                }
                paths.push(package_manifest);
            }
        }
    }
    paths.sort();
    if paths.len() > 64 {
        bail!("agent manifest directory contains more than 64 manifests");
    }

    let mut definitions = Vec::with_capacity(paths.len());
    let mut ids = BTreeSet::new();
    for path in paths {
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("agent manifest must be a regular file: {}", path.display());
        }
        if metadata.len() > 64 * 1024 {
            bail!("agent manifest exceeds 64 KiB: {}", path.display());
        }
        let input = std::fs::read_to_string(&path)?;
        let mut definition = AgentDefinition::from_toml(&input).map_err(|error| {
            anyhow::anyhow!("invalid agent manifest {}: {error}", path.display())
        })?;
        if definition.built_in {
            bail!("custom agent cannot set built_in=true: {}", path.display());
        }
        definition.built_in = false;
        if !ids.insert(definition.id.clone()) {
            bail!("duplicate custom agent id: {}", definition.id);
        }
        definitions.push(definition);
    }
    Ok(definitions)
}

/// Installs a normalized declarative package. Updates preserve the previous
/// manifest as `agent.toml.bak`; neither path may be a symlink.
pub fn install_agent_definition(
    definition: &AgentDefinition,
    directory: &Path,
    overwrite: bool,
) -> Result<std::path::PathBuf> {
    definition.validate()?;
    if definition.built_in
        || built_in_agents()
            .iter()
            .any(|built_in| built_in.id == definition.id)
    {
        bail!("built-in agent ids cannot be installed or replaced");
    }
    if directory.exists() && fs::symlink_metadata(directory)?.file_type().is_symlink() {
        bail!("agent installation directory must not be a symlink");
    }
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let package = directory.join(&definition.id);
    if package.exists() {
        let metadata = fs::symlink_metadata(&package)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("agent package destination must be a real directory");
        }
    } else {
        fs::create_dir(&package)?;
    }
    fs::set_permissions(&package, fs::Permissions::from_mode(0o700))?;
    let manifest = package.join("agent.toml");
    if manifest.exists() {
        let metadata = fs::symlink_metadata(&manifest)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("agent package manifest must be a regular file");
        }
        if !overwrite {
            bail!("agent package already exists; explicit update is required");
        }
        let backup = package.join("agent.toml.bak");
        fs::copy(&manifest, &backup)?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    }

    let encoded = definition.to_toml()?;
    let temporary = package.join(format!(".agent.toml.{}.tmp", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    output.write_all(encoded.as_bytes())?;
    output.sync_all()?;
    fs::rename(&temporary, &manifest)?;
    Ok(manifest)
}

/// Restores the package's last backed-up manifest. The displaced current
/// manifest becomes the new backup, so a mistaken rollback is reversible.
pub fn rollback_agent_definition(directory: &Path, agent_id: &str) -> Result<std::path::PathBuf> {
    if agent_id.is_empty()
        || agent_id.len() > 64
        || !agent_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("invalid agent id");
    }
    let package = directory.join(agent_id);
    let backup = package.join("agent.toml.bak");
    let metadata = fs::symlink_metadata(&backup)
        .map_err(|error| anyhow::anyhow!("agent backup is unavailable: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        bail!("agent backup must be a regular manifest no larger than 64 KiB");
    }
    let definition = AgentDefinition::from_toml(&fs::read_to_string(&backup)?)?;
    if definition.id != agent_id || definition.built_in {
        bail!("agent backup identity does not match its package");
    }
    install_agent_definition(&definition, directory, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_are_valid_and_unique() {
        let agents = built_in_agents();
        let ids = agents
            .iter()
            .map(|agent| &agent.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), agents.len());
        for agent in agents {
            agent.validate().unwrap();
        }
    }

    #[test]
    fn manifest_parser_rejects_unknown_fields_and_excessive_authority() {
        assert!(
            AgentDefinition::from_toml("manifest_version = 1\nid = 'bad'\nunknown = true").is_err()
        );
        let mut agent = built_in_agents().remove(0);
        agent.max_tool_steps = 5;
        assert!(agent.validate().is_err());
    }

    #[test]
    fn directory_loader_rejects_builtin_impersonation() {
        let directory =
            std::env::temp_dir().join(format!("focaldesk-agent-manifests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("unsafe.toml"),
            r#"
manifest_version = 1
id = "custom"
name = "Custom"
description = "test"
instructions = "test"
tool_allowlist = ["list_windows"]
built_in = true
"#,
        )
        .unwrap();
        let error = load_agent_definitions(&directory).unwrap_err();
        assert!(error.to_string().contains("built_in=true"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn directory_loader_accepts_declarative_package_directories() {
        let directory =
            std::env::temp_dir().join(format!("focaldesk-agent-packages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let package = directory.join("workspace-guide");
        std::fs::create_dir_all(&package).unwrap();
        let definition = crate::AgentBuilder::new("workspace-guide", "Workspace Guide")
            .description("Explains a workspace")
            .instructions("Use observed metadata only.")
            .allow_tool("list_windows")
            .build()
            .unwrap();
        std::fs::write(package.join("agent.toml"), definition.to_toml().unwrap()).unwrap();

        let loaded = load_agent_definitions(&directory).unwrap();
        assert_eq!(loaded, vec![definition]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn trigger_validation_requires_bounded_kind_specific_configuration() {
        let mut agent = built_in_agents().remove(0);
        agent.triggers.push(AgentTrigger {
            id: "too-fast".into(),
            kind: AgentTriggerKind::Schedule,
            objective: "Inspect the desktop".into(),
            match_value: String::new(),
            interval_seconds: Some(30),
            cooldown_seconds: 0,
            max_runs_per_hour: 60,
            enabled: true,
        });
        assert!(agent.validate().is_err());

        let mut non_voice = built_in_agents().remove(1);
        non_voice.triggers.push(AgentTrigger {
            id: "wake".into(),
            kind: AgentTriggerKind::VoicePhrase,
            objective: "Inspect the desktop".into(),
            match_value: "hello focaldesk".into(),
            interval_seconds: None,
            cooldown_seconds: 60,
            max_runs_per_hour: 6,
            enabled: true,
        });
        assert!(non_voice.validate().is_err());
    }

    #[test]
    fn installer_requires_explicit_update_and_preserves_a_recovery_copy() {
        let directory =
            std::env::temp_dir().join(format!("focaldesk-agent-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let mut definition = crate::AgentBuilder::new("install-test", "Install test")
            .allow_tool("list_windows")
            .build()
            .unwrap();
        let manifest = install_agent_definition(&definition, &directory, false).unwrap();
        assert!(install_agent_definition(&definition, &directory, false).is_err());
        definition.description = "updated".into();
        install_agent_definition(&definition, &directory, true).unwrap();
        assert!(manifest.with_file_name("agent.toml.bak").exists());
        assert_eq!(
            AgentDefinition::from_toml(&fs::read_to_string(&manifest).unwrap())
                .unwrap()
                .description,
            "updated"
        );
        rollback_agent_definition(&directory, "install-test").unwrap();
        assert_eq!(
            AgentDefinition::from_toml(&fs::read_to_string(&manifest).unwrap())
                .unwrap()
                .description,
            ""
        );
        assert_eq!(
            AgentDefinition::from_toml(
                &fs::read_to_string(manifest.with_file_name("agent.toml.bak")).unwrap()
            )
            .unwrap()
            .description,
            "updated"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
