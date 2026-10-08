use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const SCENARIO_VERSION: u16 = 1;
const MAX_SCENARIO_BYTES: usize = 192 * 1024;
const MAX_SCENARIO_STEPS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioConnectorPolicy {
    pub enabled: bool,
    pub sources: BTreeMap<crate::EventSource, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioInitialState {
    #[serde(default = "default_true")]
    pub event_fabric_connected: bool,
    #[serde(default)]
    pub routines_suspended: bool,
    #[serde(default)]
    pub source_fields: BTreeMap<crate::EventSource, Vec<String>>,
    #[serde(default)]
    pub connectors: BTreeMap<String, ScenarioConnectorPolicy>,
    #[serde(default)]
    pub agent_tools: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_token_budget")]
    pub token_budget: u64,
}

impl Default for ScenarioInitialState {
    fn default() -> Self {
        Self {
            event_fabric_connected: true,
            routines_suspended: false,
            source_fields: BTreeMap::new(),
            connectors: BTreeMap::new(),
            agent_tools: BTreeMap::new(),
            token_budget: default_token_budget(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLeaseState {
    Active,
    Missing,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioStep {
    VoicePhrase {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_destination: Option<String>,
    },
    ConnectorEvent {
        connector_id: String,
        source: crate::EventSource,
        payload: Value,
    },
    ContextMetadata {
        kind: crate::ContextKind,
        provenance: String,
        sensitivity: crate::ContextSensitivity,
        fields: Vec<String>,
    },
    RoutineEvent {
        event: crate::RoutineEvent,
    },
    AgentPlan {
        agent_id: String,
        tools: Vec<String>,
        #[serde(default)]
        proposed_mutation: bool,
        #[serde(default)]
        confirmation_present: bool,
        lease: ScenarioLeaseState,
        estimated_tokens: u64,
    },
    FailureInjection {
        component: String,
        mode: String,
    },
    Restart {
        component: String,
    },
    ObservedTimeline {
        kind: crate::MissionTimelineKind,
        title: String,
        state: String,
        provenance: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioFixture {
    pub scenario_version: u16,
    pub name: String,
    pub description: String,
    pub synthetic: bool,
    #[serde(default)]
    pub initial: ScenarioInitialState,
    pub steps: Vec<ScenarioStep>,
    #[serde(default)]
    pub expected_violation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioCheck {
    pub step_index: usize,
    pub code: String,
    pub passed: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioRouteObservation {
    pub step_index: usize,
    pub destination: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioReport {
    pub name: String,
    pub passed: bool,
    pub checks: Vec<ScenarioCheck>,
    pub observed_violation_codes: Vec<String>,
    pub expected_violation_codes: Vec<String>,
    pub unexpected_violation_codes: Vec<String>,
    pub missing_violation_codes: Vec<String>,
    pub routes: Vec<ScenarioRouteObservation>,
    pub simulated_routine_matches: usize,
    pub estimated_tokens: u64,
    pub provider_calls: usize,
    pub tool_executions: usize,
    pub live_mutations: usize,
}

impl ScenarioFixture {
    pub fn validate(&mut self) -> Result<()> {
        if self.scenario_version != SCENARIO_VERSION {
            bail!("unsupported Scenario Lab fixture version");
        }
        if !self.synthetic {
            bail!("Scenario Lab fixtures must be explicitly marked synthetic");
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.description.chars().count() > 500
            || self.steps.is_empty()
            || self.steps.len() > MAX_SCENARIO_STEPS
            || !(1..=100_000_000).contains(&self.initial.token_budget)
        {
            bail!("Scenario Lab fixture metadata or limits are out of bounds");
        }
        normalize_identifier_list(&mut self.expected_violation_codes, 64)?;
        for (source, fields) in &mut self.initial.source_fields {
            normalize_fields(fields)?;
            let supported = crate::supported_event_fields(*source);
            if fields
                .iter()
                .any(|field| !supported.contains(&field.as_str()))
            {
                bail!("scenario source policy contains an unsupported field");
            }
        }
        for (connector_id, policy) in &mut self.initial.connectors {
            validate_identifier(connector_id, "connector")?;
            for (source, fields) in &mut policy.sources {
                normalize_fields(fields)?;
                let supported = crate::supported_event_fields(*source);
                if fields
                    .iter()
                    .any(|field| !supported.contains(&field.as_str()))
                {
                    bail!("scenario connector schema contains an unsupported field");
                }
            }
        }
        for (agent_id, tools) in &mut self.initial.agent_tools {
            validate_identifier(agent_id, "agent")?;
            normalize_identifier_list(tools, 64)?;
        }
        for step in &mut self.steps {
            validate_step(step)?;
        }
        if serde_json::to_vec(self)?.len() > MAX_SCENARIO_BYTES {
            bail!("Scenario Lab fixture exceeds 192 KiB");
        }
        Ok(())
    }

    pub fn from_timeline(name: String, timeline: &[crate::MissionTimelineEntry]) -> Result<Self> {
        let mut fixture = Self {
            scenario_version: SCENARIO_VERSION,
            name,
            description: "Privacy-minimized Mission Control trace; observed entries are inert."
                .into(),
            synthetic: true,
            initial: ScenarioInitialState::default(),
            steps: timeline
                .iter()
                .take(MAX_SCENARIO_STEPS)
                .map(|entry| ScenarioStep::ObservedTimeline {
                    kind: entry.kind,
                    title: entry.title.clone(),
                    state: entry.state.clone(),
                    provenance: entry.provenance.clone(),
                })
                .collect(),
            expected_violation_codes: Vec::new(),
        };
        if fixture.steps.is_empty() {
            fixture.steps.push(ScenarioStep::ObservedTimeline {
                kind: crate::MissionTimelineKind::Control,
                title: "empty trace".into(),
                state: "observed".into(),
                provenance: "mission-control".into(),
            });
        }
        fixture.validate()?;
        Ok(fixture)
    }
}

pub fn evaluate_scenario(mut fixture: ScenarioFixture) -> Result<ScenarioReport> {
    fixture.validate()?;
    let mut checks = Vec::new();
    let mut routes = Vec::new();
    let mut estimated_tokens = 0_u64;
    let mut routine_matches = 0_usize;
    let mut injected_failures = BTreeMap::<String, String>::new();
    let routine_engine = crate::RoutineEngine::default();
    routine_engine.set_suspended(fixture.initial.routines_suspended)?;

    for (index, step) in fixture.steps.iter().enumerate() {
        match step {
            ScenarioStep::VoicePhrase {
                text,
                expected_destination,
            } => {
                let route = crate::route_intent(text);
                let destination = destination_name(&route.destination);
                routes.push(ScenarioRouteObservation {
                    step_index: index,
                    destination: destination.clone(),
                    reason: route.reason,
                });
                if let Some(expected) = expected_destination {
                    check(
                        &mut checks,
                        index,
                        "voice_route_mismatch",
                        destination == *expected,
                        format!("expected={expected} observed={destination}"),
                    );
                }
            }
            ScenarioStep::ConnectorEvent {
                connector_id,
                source,
                payload,
            } => {
                check(
                    &mut checks,
                    index,
                    "event_fabric_disconnected",
                    fixture.initial.event_fabric_connected,
                    "connector event requires connected Event Fabric".into(),
                );
                let allowed = fixture.initial.source_fields.get(source);
                check(
                    &mut checks,
                    index,
                    "source_without_consent",
                    allowed.is_some_and(|fields| !fields.is_empty()),
                    format!(
                        "source={} must have an enabled disclosure policy",
                        source.as_str()
                    ),
                );
                let connector = fixture.initial.connectors.get(connector_id);
                check(
                    &mut checks,
                    index,
                    "connector_disabled",
                    connector.is_some_and(|policy| policy.enabled),
                    format!("connector={connector_id} must be enabled"),
                );
                check(
                    &mut checks,
                    index,
                    "connector_unavailable",
                    !injected_failures.contains_key("connector")
                        && !injected_failures.contains_key(connector_id),
                    format!("connector={connector_id} must not have an injected failure"),
                );
                let connector_fields = connector.and_then(|policy| policy.sources.get(source));
                check(
                    &mut checks,
                    index,
                    "connector_source_undeclared",
                    connector_fields.is_some(),
                    format!(
                        "connector={connector_id} must declare source={}",
                        source.as_str()
                    ),
                );
                let payload_fields = payload
                    .as_object()
                    .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>())
                    .unwrap_or_default();
                check(
                    &mut checks,
                    index,
                    "payload_missing_disclosed_field",
                    allowed.is_some_and(|fields| {
                        fields
                            .iter()
                            .any(|field| payload_fields.contains(field.as_str()))
                    }),
                    "payload must contain at least one source-disclosed field".into(),
                );
                check(
                    &mut checks,
                    index,
                    "payload_disclosed_field_non_scalar",
                    allowed.is_some_and(|fields| {
                        payload.as_object().is_some_and(|payload| {
                            fields.iter().all(|field| {
                                payload.get(field).is_none_or(|value| {
                                    value.is_null()
                                        || value.is_boolean()
                                        || value.is_number()
                                        || value.is_string()
                                })
                            })
                        })
                    }),
                    "source-disclosed payload fields must be scalar".into(),
                );
                check(
                    &mut checks,
                    index,
                    "payload_outside_connector_schema",
                    connector_fields.is_some_and(|fields| {
                        payload_fields
                            .iter()
                            .all(|field| fields.iter().any(|item| item == field))
                    }),
                    "payload fields must be declared by the connector".into(),
                );
            }
            ScenarioStep::ContextMetadata { fields, .. } => check(
                &mut checks,
                index,
                "context_metadata_unbounded",
                fields.len() <= 32,
                "context fixtures contain field names only".into(),
            ),
            ScenarioStep::RoutineEvent { event } => {
                let evaluations = routine_engine.simulate(event.clone())?;
                routine_matches += evaluations.iter().filter(|item| item.matched).count();
                check(
                    &mut checks,
                    index,
                    "routine_live_mutation",
                    evaluations.iter().all(|item| item.simulated),
                    "routine evaluation must remain simulated".into(),
                );
            }
            ScenarioStep::AgentPlan {
                agent_id,
                tools,
                proposed_mutation,
                confirmation_present,
                lease,
                estimated_tokens: step_tokens,
            } => {
                let allowed = fixture.initial.agent_tools.get(agent_id);
                check(
                    &mut checks,
                    index,
                    "tool_outside_allowlist",
                    allowed.is_some_and(|allowed| tools.iter().all(|tool| allowed.contains(tool))),
                    format!("agent={agent_id} plan tools must be allowlisted"),
                );
                check(
                    &mut checks,
                    index,
                    "provider_unavailable",
                    !injected_failures.contains_key("provider"),
                    "agent planning requires an available shadow provider".into(),
                );
                check(
                    &mut checks,
                    index,
                    "inactive_capability_lease",
                    *lease == ScenarioLeaseState::Active,
                    format!("lease state is {lease:?}"),
                );
                check(
                    &mut checks,
                    index,
                    "mutation_without_confirmation",
                    !*proposed_mutation || *confirmation_present,
                    "mutating plans require an explicit native confirmation boundary".into(),
                );
                estimated_tokens = estimated_tokens.saturating_add(*step_tokens);
                check(
                    &mut checks,
                    index,
                    "token_budget_exceeded",
                    estimated_tokens <= fixture.initial.token_budget,
                    format!(
                        "estimated_tokens={estimated_tokens} budget={}",
                        fixture.initial.token_budget
                    ),
                );
            }
            ScenarioStep::FailureInjection { component, mode } => {
                injected_failures.insert(component.clone(), mode.clone());
                check(
                    &mut checks,
                    index,
                    "failure_injection_not_isolated",
                    true,
                    format!("isolated failure component={component} mode={mode}"),
                );
            }
            ScenarioStep::Restart { component } => {
                if component == "all" || component == "ai-service" {
                    injected_failures.clear();
                } else {
                    injected_failures.remove(component);
                }
                check(
                    &mut checks,
                    index,
                    "restart_not_isolated",
                    true,
                    format!("shadow restart component={component}"),
                );
            }
            ScenarioStep::ObservedTimeline { .. } => check(
                &mut checks,
                index,
                "observed_trace_mutation",
                true,
                "observed timeline entry is inert".into(),
            ),
        }
    }

    let observed = checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.code.clone())
        .collect::<BTreeSet<_>>();
    let expected = fixture
        .expected_violation_codes
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let unexpected = observed.difference(&expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(&observed).cloned().collect::<Vec<_>>();
    Ok(ScenarioReport {
        name: fixture.name,
        passed: unexpected.is_empty() && missing.is_empty(),
        checks,
        observed_violation_codes: observed.into_iter().collect(),
        expected_violation_codes: expected.into_iter().collect(),
        unexpected_violation_codes: unexpected,
        missing_violation_codes: missing,
        routes,
        simulated_routine_matches: routine_matches,
        estimated_tokens,
        provider_calls: 0,
        tool_executions: 0,
        live_mutations: 0,
    })
}

fn validate_step(step: &mut ScenarioStep) -> Result<()> {
    match step {
        ScenarioStep::VoicePhrase {
            text,
            expected_destination,
        } => {
            validate_text(text, 500, "voice phrase")?;
            if let Some(destination) = expected_destination {
                validate_text(destination, 100, "voice destination")?;
            }
        }
        ScenarioStep::ConnectorEvent {
            connector_id,
            payload,
            ..
        } => {
            validate_identifier(connector_id, "connector")?;
            if !payload.is_object() || serde_json::to_vec(payload)?.len() > 16 * 1024 {
                bail!("scenario connector payload must be a JSON object no larger than 16 KiB");
            }
        }
        ScenarioStep::ContextMetadata {
            provenance, fields, ..
        } => {
            validate_text(provenance, 200, "context provenance")?;
            normalize_fields(fields)?;
            if fields.len() > 32 {
                bail!("scenario context metadata may declare at most 32 field names");
            }
        }
        ScenarioStep::RoutineEvent { event } => {
            validate_text(&event.value, 2_000, "routine value")?;
            validate_text(&event.source, 200, "routine source")?;
        }
        ScenarioStep::AgentPlan {
            agent_id,
            tools,
            estimated_tokens,
            ..
        } => {
            validate_identifier(agent_id, "agent")?;
            normalize_identifier_list(tools, 64)?;
            if *estimated_tokens > 100_000_000 {
                bail!("scenario token estimate is out of bounds");
            }
        }
        ScenarioStep::FailureInjection { component, mode } => {
            validate_text(component, 100, "failure component")?;
            validate_text(mode, 100, "failure mode")?;
        }
        ScenarioStep::Restart { component } => {
            validate_text(component, 100, "restart component")?;
        }
        ScenarioStep::ObservedTimeline {
            title,
            state,
            provenance,
            ..
        } => {
            validate_text(title, 120, "timeline title")?;
            validate_text(state, 80, "timeline state")?;
            validate_text(provenance, 200, "timeline provenance")?;
        }
    }
    Ok(())
}

fn check(
    checks: &mut Vec<ScenarioCheck>,
    step_index: usize,
    code: &str,
    passed: bool,
    summary: String,
) {
    checks.push(ScenarioCheck {
        step_index,
        code: code.into(),
        passed,
        summary,
    });
}

fn destination_name(destination: &crate::IntentDestination) -> String {
    match destination {
        crate::IntentDestination::Chat => "chat".into(),
        crate::IntentDestination::Agent { agent_id } => format!("agent:{agent_id}"),
        crate::IntentDestination::Workflow { workflow_id } => format!("workflow:{workflow_id}"),
        crate::IntentDestination::SuggestionInbox => "suggestion_inbox".into(),
    }
}

fn normalize_fields(fields: &mut Vec<String>) -> Result<()> {
    fields.sort();
    fields.dedup();
    if fields.is_empty()
        || fields.len() > 64
        || fields.iter().any(|field| {
            field.is_empty()
                || field.len() > 80
                || !field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        bail!("scenario field list is invalid or out of bounds");
    }
    Ok(())
}

fn normalize_identifier_list(values: &mut Vec<String>, maximum: usize) -> Result<()> {
    values.sort();
    values.dedup();
    if values.len() > maximum {
        bail!("scenario identifier list exceeds its bound");
    }
    for value in values.iter() {
        validate_identifier(value, "scenario")?;
    }
    Ok(())
}

fn validate_identifier(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(anyhow!("{kind} identifier is invalid"));
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, kind: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > maximum {
        bail!("scenario {kind} is empty or out of bounds");
    }
    Ok(())
}

const fn default_true() -> bool {
    true
}

const fn default_token_budget() -> u64 {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn safe_fixture() -> ScenarioFixture {
        ScenarioFixture {
            scenario_version: SCENARIO_VERSION,
            name: "safe-connector-and-agent".into(),
            description: "Synthetic safe-path contract".into(),
            synthetic: true,
            initial: ScenarioInitialState {
                source_fields: BTreeMap::from([(
                    crate::EventSource::Calendar,
                    vec!["event".into(), "title".into()],
                )]),
                connectors: BTreeMap::from([(
                    "calendar-fixture".into(),
                    ScenarioConnectorPolicy {
                        enabled: true,
                        sources: BTreeMap::from([(
                            crate::EventSource::Calendar,
                            vec!["event".into(), "title".into()],
                        )]),
                    },
                )]),
                agent_tools: BTreeMap::from([("desktop".into(), vec!["desktop_snapshot".into()])]),
                ..Default::default()
            },
            steps: vec![
                ScenarioStep::ConnectorEvent {
                    connector_id: "calendar-fixture".into(),
                    source: crate::EventSource::Calendar,
                    payload: json!({"event":"calendar meeting","title":"Synthetic"}),
                },
                ScenarioStep::AgentPlan {
                    agent_id: "desktop".into(),
                    tools: vec!["desktop_snapshot".into()],
                    proposed_mutation: false,
                    confirmation_present: false,
                    lease: ScenarioLeaseState::Active,
                    estimated_tokens: 500,
                },
            ],
            expected_violation_codes: Vec::new(),
        }
    }

    #[test]
    fn safe_fixture_runs_without_live_execution() {
        let report = evaluate_scenario(safe_fixture()).unwrap();
        assert!(report.passed);
        assert_eq!(report.provider_calls, 0);
        assert_eq!(report.tool_executions, 0);
        assert_eq!(report.live_mutations, 0);
    }

    #[test]
    fn unsafe_fixture_reports_expected_invariants() {
        let mut fixture = safe_fixture();
        fixture.steps = vec![ScenarioStep::AgentPlan {
            agent_id: "desktop".into(),
            tools: vec!["delete_file".into()],
            proposed_mutation: true,
            confirmation_present: false,
            lease: ScenarioLeaseState::Expired,
            estimated_tokens: 20_000,
        }];
        fixture.expected_violation_codes = vec![
            "inactive_capability_lease".into(),
            "mutation_without_confirmation".into(),
            "token_budget_exceeded".into(),
            "tool_outside_allowlist".into(),
        ];
        assert!(evaluate_scenario(fixture).unwrap().passed);
    }

    #[test]
    fn unmarked_fixture_is_rejected() {
        let mut fixture = safe_fixture();
        fixture.synthetic = false;
        assert!(evaluate_scenario(fixture).is_err());
    }

    #[test]
    fn injected_provider_failure_blocks_planning_until_restart() {
        let mut fixture = safe_fixture();
        fixture.steps = vec![
            ScenarioStep::FailureInjection {
                component: "provider".into(),
                mode: "unavailable".into(),
            },
            ScenarioStep::AgentPlan {
                agent_id: "desktop".into(),
                tools: vec!["desktop_snapshot".into()],
                proposed_mutation: false,
                confirmation_present: false,
                lease: ScenarioLeaseState::Active,
                estimated_tokens: 100,
            },
        ];
        fixture.expected_violation_codes = vec!["provider_unavailable".into()];
        assert!(evaluate_scenario(fixture).unwrap().passed);

        let mut recovered = safe_fixture();
        recovered.steps = vec![
            ScenarioStep::FailureInjection {
                component: "provider".into(),
                mode: "unavailable".into(),
            },
            ScenarioStep::Restart {
                component: "provider".into(),
            },
            ScenarioStep::AgentPlan {
                agent_id: "desktop".into(),
                tools: vec!["desktop_snapshot".into()],
                proposed_mutation: false,
                confirmation_present: false,
                lease: ScenarioLeaseState::Active,
                estimated_tokens: 100,
            },
        ];
        assert!(evaluate_scenario(recovered).unwrap().passed);
    }
}
