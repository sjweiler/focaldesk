use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const WORKFLOW_MANIFEST_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowNode {
    pub id: String,
    pub agent_id: String,
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub manifest_version: u16,
    pub id: String,
    pub name: String,
    pub description: String,
    pub nodes: Vec<WorkflowNode>,
    pub max_parallelism: usize,
    pub timeout_seconds: u64,
    pub max_total_tokens: u64,
    #[serde(default)]
    pub capability_ceiling: Option<crate::CapabilityPolicy>,
    #[serde(default)]
    pub built_in: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowRunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowArtifact {
    pub node_id: String,
    pub media_type: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeStatus {
    pub node_id: String,
    pub state: WorkflowNodeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunStatus {
    pub run_id: String,
    pub workflow_id: String,
    pub state: WorkflowRunState,
    pub created_at_unix: u64,
    pub deadline_at_unix: u64,
    pub max_total_tokens: u64,
    pub total_tokens: u64,
    pub nodes: BTreeMap<String, WorkflowNodeStatus>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, WorkflowArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WorkflowDefinition {
    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != WORKFLOW_MANIFEST_VERSION {
            bail!("unsupported workflow manifest version");
        }
        validate_id(&self.id, "workflow")?;
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            bail!("workflow name must contain 1-80 characters");
        }
        if self.description.chars().count() > 500 || !(1..=16).contains(&self.nodes.len()) {
            bail!("workflow description or node count exceeds its bound");
        }
        if !(1..=4).contains(&self.max_parallelism)
            || !(30..=3_600).contains(&self.timeout_seconds)
            || !(1_000..=100_000_000).contains(&self.max_total_tokens)
        {
            bail!("workflow parallelism, deadline, or token budget is out of bounds");
        }
        if let Some(policy) = &self.capability_ceiling {
            policy.validate()?;
        }
        let ids = self
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        if ids.len() != self.nodes.len() {
            bail!("workflow node ids must be unique");
        }
        for node in &self.nodes {
            validate_id(&node.id, "workflow node")?;
            validate_id(&node.agent_id, "agent")?;
            if node.objective.trim().is_empty() || node.objective.chars().count() > 2_000 {
                bail!("workflow node objective must contain 1-2000 characters");
            }
            let dependencies = node.depends_on.iter().collect::<BTreeSet<_>>();
            if dependencies.len() != node.depends_on.len()
                || node
                    .depends_on
                    .iter()
                    .any(|dependency| !ids.contains(dependency.as_str()))
                || node.depends_on.contains(&node.id)
            {
                bail!("workflow node has an invalid dependency");
            }
        }
        let dependencies = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.depends_on.as_slice()))
            .collect::<BTreeMap<_, _>>();
        for node in &self.nodes {
            let mut visiting = BTreeSet::new();
            ensure_acyclic(&node.id, &dependencies, &mut visiting)?;
        }
        Ok(())
    }
}

fn ensure_acyclic<'a>(
    node: &'a str,
    dependencies: &BTreeMap<&'a str, &'a [String]>,
    visiting: &mut BTreeSet<&'a str>,
) -> Result<()> {
    if !visiting.insert(node) {
        bail!("workflow dependency graph contains a cycle");
    }
    if let Some(items) = dependencies.get(node) {
        for dependency in *items {
            ensure_acyclic(dependency, dependencies, visiting)?;
        }
    }
    visiting.remove(node);
    Ok(())
}

fn validate_id(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{kind} id must contain 1-64 lowercase ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

pub fn built_in_workflows() -> Vec<WorkflowDefinition> {
    vec![
        WorkflowDefinition {
            manifest_version: WORKFLOW_MANIFEST_VERSION,
            id: "morning-briefing".into(),
            name: "Morning Briefing".into(),
            description: "Inspects the current session and produces a concise voice-friendly briefing.".into(),
            nodes: vec![
                WorkflowNode { id: "inspect".into(), agent_id: "desktop".into(), objective: "Inspect the current session, workspaces, and open windows. Report only observed facts and pending issues.".into(), depends_on: vec![] },
                WorkflowNode { id: "brief".into(), agent_id: "accessibility".into(), objective: "Turn the supplied inspection artifact into a short spoken morning briefing. Do not invent events or tasks.".into(), depends_on: vec!["inspect".into()] },
            ],
            max_parallelism: 2,
            timeout_seconds: 300,
            max_total_tokens: 8_000,
            capability_ceiling: None,
            built_in: true,
        },
        WorkflowDefinition {
            manifest_version: WORKFLOW_MANIFEST_VERSION,
            id: "workspace-troubleshooter".into(),
            name: "Workspace Troubleshooter".into(),
            description: "Collects desktop and diagnostic evidence in parallel, then summarizes it.".into(),
            nodes: vec![
                WorkflowNode { id: "desktop-state".into(), agent_id: "desktop".into(), objective: "Inspect current windows, workspaces, outputs, and session state for visible anomalies.".into(), depends_on: vec![] },
                WorkflowNode { id: "diagnostics".into(), agent_id: "troubleshooter".into(), objective: "Inspect service health, rendering status, and recent diagnostic evidence for failures.".into(), depends_on: vec![] },
                WorkflowNode { id: "summary".into(), agent_id: "troubleshooter".into(), objective: "Correlate the supplied typed artifacts. Separate observations, likely causes, and safe next checks.".into(), depends_on: vec!["desktop-state".into(), "diagnostics".into()] },
            ],
            max_parallelism: 2,
            timeout_seconds: 420,
            max_total_tokens: 12_000,
            capability_ceiling: None,
            built_in: true,
        },
        WorkflowDefinition {
            manifest_version: WORKFLOW_MANIFEST_VERSION,
            id: "meeting-preparation".into(),
            name: "Meeting Preparation".into(),
            description: "Summarizes the visible workspace without claiming calendar or meeting access.".into(),
            nodes: vec![
                WorkflowNode { id: "workspace".into(), agent_id: "desktop".into(), objective: "Inspect the visible workspace and identify windows or documents that appear relevant to the user's next meeting. State access limitations.".into(), depends_on: vec![] },
                WorkflowNode { id: "check".into(), agent_id: "troubleshooter".into(), objective: "Review the supplied workspace artifact for missing evidence, unhealthy services, or presentation risks.".into(), depends_on: vec!["workspace".into()] },
            ],
            max_parallelism: 1,
            timeout_seconds: 300,
            max_total_tokens: 8_000,
            capability_ceiling: None,
            built_in: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_are_bounded_and_acyclic() {
        for workflow in built_in_workflows() {
            workflow.validate().unwrap();
        }
    }

    #[test]
    fn cycles_are_rejected() {
        let mut workflow = built_in_workflows().remove(0);
        workflow.nodes[0].depends_on = vec!["brief".into()];
        assert!(
            workflow
                .validate()
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }
}
