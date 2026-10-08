use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MissionTimelineKind {
    Event,
    Routine,
    Context,
    Agent,
    Workflow,
    Control,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionTimelineEntry {
    pub id: String,
    pub at_unix: u64,
    pub kind: MissionTimelineKind,
    pub title: String,
    pub summary: String,
    pub provenance: String,
    pub state: String,
    #[serde(default)]
    pub simulated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionRuntimeSummary {
    pub run_id: String,
    pub kind: String,
    pub owner_id: String,
    pub state: String,
    pub created_at_unix: u64,
    pub deadline_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionBudgetSummary {
    pub agent_id: String,
    pub enabled: bool,
    pub runs_today: usize,
    pub failures_today: usize,
    pub tokens_used_today: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<u64>,
    pub cost_used_microusd_today: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_limit_microusd: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionConnectorSummary {
    pub connector_id: String,
    pub enabled: bool,
    pub health: String,
    pub network_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionControlSnapshot {
    pub generated_at_unix: u64,
    pub globally_paused: bool,
    pub triggers_suspended: bool,
    pub routines_suspended: bool,
    pub event_fabric_connected: bool,
    pub active_runs: Vec<MissionRuntimeSummary>,
    pub active_leases: Vec<crate::CapabilityLease>,
    pub active_context_grants: Vec<crate::ContextGrant>,
    pub budgets: Vec<MissionBudgetSummary>,
    pub connectors: Vec<MissionConnectorSummary>,
    pub enabled_connectors: Vec<String>,
    pub pending_suggestions: usize,
    pub timeline: Vec<MissionTimelineEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct MissionAuditRecord {
    pub sequence: u64,
    pub at_unix: u64,
    pub agent_id: Option<String>,
    pub action: String,
    pub details: String,
}
