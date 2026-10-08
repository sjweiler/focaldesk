use anyhow::{Result, anyhow, bail};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SUGGESTIONS: usize = 64;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RoutineEventKind {
    Desktop,
    Calendar,
    Notification,
    Service,
    Workflow,
    Context,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionPriority {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutinePromotion {
    Agent { agent_id: String },
    Workflow { workflow_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuietHours {
    pub start_hour_utc: u8,
    pub end_hour_utc: u8,
}

impl QuietHours {
    fn contains(&self, hour: u8) -> bool {
        if self.start_hour_utc == self.end_hour_utc {
            return true;
        }
        if self.start_hour_utc < self.end_hour_utc {
            (self.start_hour_utc..self.end_hour_utc).contains(&hour)
        } else {
            hour >= self.start_hour_utc || hour < self.end_hour_utc
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub event_kind: RoutineEventKind,
    pub match_contains: String,
    pub suggestion_title: String,
    pub suggestion_body: String,
    pub priority: AttentionPriority,
    pub promotion: RoutinePromotion,
    pub cooldown_seconds: u64,
    pub max_suggestions_per_hour: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHours>,
}

impl RoutineDefinition {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id, "routine")?;
        if self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self.description.trim().is_empty()
            || self.description.chars().count() > 500
            || self.match_contains.trim().is_empty()
            || self.match_contains.chars().count() > 200
            || self.suggestion_title.trim().is_empty()
            || self.suggestion_title.chars().count() > 120
            || self.suggestion_body.trim().is_empty()
            || self.suggestion_body.chars().count() > 2_000
            || !(30..=86_400).contains(&self.cooldown_seconds)
            || !(1..=20).contains(&self.max_suggestions_per_hour)
        {
            bail!("routine definition is out of bounds");
        }
        match &self.promotion {
            RoutinePromotion::Agent { agent_id } => validate_id(agent_id, "agent")?,
            RoutinePromotion::Workflow { workflow_id } => validate_id(workflow_id, "workflow")?,
        }
        if let Some(quiet) = &self.quiet_hours
            && (quiet.start_hour_utc > 23 || quiet.end_hour_utc > 23)
        {
            bail!("quiet-hour values must be between 0 and 23 UTC");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineEvent {
    pub kind: RoutineEventKind,
    pub value: String,
    pub source: String,
}

impl RoutineEvent {
    fn validate(&self) -> Result<()> {
        if self.value.trim().is_empty()
            || self.value.chars().count() > 2_000
            || self.source.trim().is_empty()
            || self.source.chars().count() > 200
        {
            bail!("routine event is out of bounds");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineSuggestion {
    pub id: String,
    pub routine_id: String,
    pub title: String,
    pub body: String,
    pub priority: AttentionPriority,
    pub trigger_kind: RoutineEventKind,
    pub trigger_source: String,
    pub trigger_value: String,
    pub reason: String,
    pub promotion: RoutinePromotion,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub dismissed: bool,
    pub promoted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineEvaluation {
    pub routine_id: String,
    pub matched: bool,
    pub simulated: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<RoutineSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineStateSnapshot {
    pub suspended: bool,
    pub definitions: Vec<RoutineDefinition>,
    pub suggestions: Vec<RoutineSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutinePromotionOutcome {
    pub suggestion_id: String,
    pub promotion: RoutinePromotion,
    pub run_id: String,
}

#[derive(Default)]
struct RoutineState {
    suspended: bool,
    suggestions: BTreeMap<String, RoutineSuggestion>,
    firings: BTreeMap<String, Vec<u64>>,
    dedupe: BTreeMap<(String, String), u64>,
}

pub struct RoutineEngine {
    definitions: RwLock<Vec<RoutineDefinition>>,
    state: Mutex<RoutineState>,
}

impl Default for RoutineEngine {
    fn default() -> Self {
        Self {
            definitions: RwLock::new(built_in_routines()),
            state: Mutex::new(RoutineState::default()),
        }
    }
}

impl RoutineEngine {
    pub fn definitions(&self) -> Vec<RoutineDefinition> {
        self.definitions
            .read()
            .map(|definitions| definitions.clone())
            .unwrap_or_default()
    }

    pub fn replace_package_definitions(&self, package: Vec<RoutineDefinition>) -> Result<()> {
        let mut definitions = built_in_routines();
        let built_in_ids = definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for definition in &package {
            definition.validate()?;
            if built_in_ids.contains(definition.id.as_str()) {
                bail!("package routine conflicts with a built-in routine");
            }
        }
        let unique = package
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != package.len() {
            bail!("package routine ids must be unique");
        }
        definitions.extend(package);
        *self
            .definitions
            .write()
            .map_err(|_| anyhow!("routine registry unavailable"))? = definitions;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<RoutineStateSnapshot> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?;
        prune(&mut state, now);
        Ok(RoutineStateSnapshot {
            suspended: state.suspended,
            definitions: self.definitions(),
            suggestions: state.suggestions.values().cloned().collect(),
        })
    }

    pub fn set_suspended(&self, suspended: bool) -> Result<bool> {
        self.state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?
            .suspended = suspended;
        Ok(suspended)
    }

    pub fn dispatch(&self, event: RoutineEvent) -> Result<Vec<RoutineEvaluation>> {
        self.evaluate(event, false, unix_now())
    }

    pub fn simulate(&self, event: RoutineEvent) -> Result<Vec<RoutineEvaluation>> {
        self.evaluate(event, true, unix_now())
    }

    fn evaluate(
        &self,
        event: RoutineEvent,
        simulated: bool,
        now: u64,
    ) -> Result<Vec<RoutineEvaluation>> {
        event.validate()?;
        let definitions = self
            .definitions
            .read()
            .map_err(|_| anyhow!("routine registry unavailable"))?
            .clone();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?;
        prune(&mut state, now);
        let mut evaluations = Vec::new();
        for definition in &definitions {
            definition.validate()?;
            if definition.event_kind != event.kind {
                continue;
            }
            let needle = definition.match_contains.to_ascii_lowercase();
            if !event.value.to_ascii_lowercase().contains(&needle) {
                evaluations.push(evaluation(
                    definition,
                    simulated,
                    "event did not match",
                    None,
                ));
                continue;
            }
            let reason = suppression_reason(&state, definition, &event, now);
            if let Some(reason) = reason {
                evaluations.push(evaluation(definition, simulated, &reason, None));
                continue;
            }
            let suggestion = make_suggestion(definition, &event, now);
            evaluations.push(evaluation(
                definition,
                simulated,
                if simulated {
                    "would publish a suggestion"
                } else {
                    "published a suggestion; no action was started"
                },
                Some(suggestion.clone()),
            ));
            if !simulated {
                while state.suggestions.len() >= MAX_SUGGESTIONS {
                    let Some(oldest) = state
                        .suggestions
                        .values()
                        .min_by_key(|item| item.created_at_unix)
                        .map(|item| item.id.clone())
                    else {
                        break;
                    };
                    state.suggestions.remove(&oldest);
                }
                state
                    .firings
                    .entry(definition.id.clone())
                    .or_default()
                    .push(now);
                state
                    .dedupe
                    .insert((definition.id.clone(), normalize(&event.value)), now);
                state.suggestions.insert(suggestion.id.clone(), suggestion);
            }
        }
        Ok(evaluations)
    }

    pub fn dismiss(&self, suggestion_id: &str) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?;
        let Some(suggestion) = state.suggestions.get_mut(suggestion_id) else {
            return Ok(false);
        };
        suggestion.dismissed = true;
        Ok(true)
    }

    pub fn claim_promotion(&self, suggestion_id: &str) -> Result<RoutineSuggestion> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?;
        prune(&mut state, now);
        let suggestion = state
            .suggestions
            .get_mut(suggestion_id)
            .ok_or_else(|| anyhow!("unknown or expired routine suggestion"))?;
        if suggestion.dismissed || suggestion.promoted {
            bail!("routine suggestion is no longer actionable");
        }
        suggestion.promoted = true;
        Ok(suggestion.clone())
    }

    pub fn release_promotion(&self, suggestion_id: &str) -> Result<()> {
        if let Some(suggestion) = self
            .state
            .lock()
            .map_err(|_| anyhow!("routine engine unavailable"))?
            .suggestions
            .get_mut(suggestion_id)
        {
            suggestion.promoted = false;
        }
        Ok(())
    }
}

fn suppression_reason(
    state: &RoutineState,
    definition: &RoutineDefinition,
    event: &RoutineEvent,
    now: u64,
) -> Option<String> {
    if state.suspended {
        return Some("all routines are paused by the emergency switch".into());
    }
    let hour_utc = ((now / 3_600) % 24) as u8;
    if definition
        .quiet_hours
        .as_ref()
        .is_some_and(|quiet| quiet.contains(hour_utc))
    {
        return Some("suppressed during configured quiet hours (UTC)".into());
    }
    let recent = state
        .firings
        .get(&definition.id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if recent
        .iter()
        .any(|fired| now.saturating_sub(*fired) < definition.cooldown_seconds)
    {
        return Some("suppressed by routine cooldown".into());
    }
    if recent
        .iter()
        .filter(|fired| now.saturating_sub(**fired) < 3_600)
        .count()
        >= definition.max_suggestions_per_hour
    {
        return Some("suppressed by hourly suggestion limit".into());
    }
    if state
        .dedupe
        .get(&(definition.id.clone(), normalize(&event.value)))
        .is_some_and(|seen| now.saturating_sub(*seen) < definition.cooldown_seconds)
    {
        return Some("suppressed as a duplicate event".into());
    }
    None
}

fn evaluation(
    definition: &RoutineDefinition,
    simulated: bool,
    reason: &str,
    suggestion: Option<RoutineSuggestion>,
) -> RoutineEvaluation {
    RoutineEvaluation {
        routine_id: definition.id.clone(),
        matched: suggestion.is_some(),
        simulated,
        reason: reason.into(),
        suggestion,
    }
}

fn make_suggestion(
    definition: &RoutineDefinition,
    event: &RoutineEvent,
    now: u64,
) -> RoutineSuggestion {
    RoutineSuggestion {
        id: random_id("routine-suggestion"),
        routine_id: definition.id.clone(),
        title: definition.suggestion_title.clone(),
        body: definition
            .suggestion_body
            .replace("{event}", event.value.trim()),
        priority: definition.priority,
        trigger_kind: event.kind,
        trigger_source: event.source.clone(),
        trigger_value: event.value.clone(),
        reason: format!(
            "{} matched '{}' from {}",
            definition.name, definition.match_contains, event.source
        ),
        promotion: definition.promotion.clone(),
        created_at_unix: now,
        expires_at_unix: now.saturating_add(86_400),
        dismissed: false,
        promoted: false,
    }
}

fn prune(state: &mut RoutineState, now: u64) {
    state
        .suggestions
        .retain(|_, suggestion| suggestion.expires_at_unix > now);
    state
        .firings
        .values_mut()
        .for_each(|firings| firings.retain(|fired| now.saturating_sub(*fired) < 86_400));
    state
        .dedupe
        .retain(|_, seen| now.saturating_sub(*seen) < 86_400);
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
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

pub fn built_in_routines() -> Vec<RoutineDefinition> {
    vec![
        RoutineDefinition {
            id: "morning-briefing".into(),
            name: "Morning briefing".into(),
            description: "Offers the existing morning briefing workflow after a typed morning event.".into(),
            event_kind: RoutineEventKind::Context,
            match_contains: "morning".into(),
            suggestion_title: "Prepare a morning briefing?".into(),
            suggestion_body: "A morning context event arrived: {event}".into(),
            priority: AttentionPriority::Normal,
            promotion: RoutinePromotion::Workflow { workflow_id: "morning-briefing".into() },
            cooldown_seconds: 14_400,
            max_suggestions_per_hour: 1,
            quiet_hours: Some(QuietHours { start_hour_utc: 22, end_hour_utc: 6 }),
        },
        RoutineDefinition {
            id: "meeting-preparation".into(),
            name: "Meeting preparation".into(),
            description: "Offers meeting preparation when an explicitly supplied calendar event mentions a meeting.".into(),
            event_kind: RoutineEventKind::Calendar,
            match_contains: "meeting".into(),
            suggestion_title: "Prepare for the meeting?".into(),
            suggestion_body: "Calendar context indicates: {event}".into(),
            priority: AttentionPriority::Normal,
            promotion: RoutinePromotion::Workflow { workflow_id: "meeting-preparation".into() },
            cooldown_seconds: 1_800,
            max_suggestions_per_hour: 2,
            quiet_hours: Some(QuietHours { start_hour_utc: 22, end_hour_utc: 6 }),
        },
        RoutineDefinition {
            id: "repeated-service-failure".into(),
            name: "Repeated service failure".into(),
            description: "Offers troubleshooting when a typed service event reports repeated failure.".into(),
            event_kind: RoutineEventKind::Service,
            match_contains: "repeated failure".into(),
            suggestion_title: "Investigate repeated service failures?".into(),
            suggestion_body: "Service health reported: {event}".into(),
            priority: AttentionPriority::High,
            promotion: RoutinePromotion::Workflow { workflow_id: "workspace-troubleshooter".into() },
            cooldown_seconds: 900,
            max_suggestions_per_hour: 3,
            quiet_hours: None,
        },
    ]
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{prefix}-{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_event() -> RoutineEvent {
        RoutineEvent {
            kind: RoutineEventKind::Service,
            value: "renderer repeated failure detected".into(),
            source: "test health stream".into(),
        }
    }

    #[test]
    fn dispatch_only_publishes_an_inert_suggestion() {
        let engine = RoutineEngine::default();
        let result = engine.dispatch(service_event()).unwrap();
        let suggestion = result.into_iter().find_map(|item| item.suggestion).unwrap();
        assert!(!suggestion.promoted);
        assert!(!suggestion.dismissed);
        assert_eq!(engine.snapshot().unwrap().suggestions.len(), 1);
    }

    #[test]
    fn simulation_does_not_mutate_engine_state() {
        let engine = RoutineEngine::default();
        assert!(
            engine
                .simulate(service_event())
                .unwrap()
                .iter()
                .any(|item| item.matched && item.simulated)
        );
        assert!(engine.snapshot().unwrap().suggestions.is_empty());
    }

    #[test]
    fn emergency_pause_suppresses_matching_events() {
        let engine = RoutineEngine::default();
        engine.set_suspended(true).unwrap();
        let result = engine.dispatch(service_event()).unwrap();
        assert!(result.iter().all(|item| item.suggestion.is_none()));
        assert!(result.iter().any(|item| item.reason.contains("emergency")));
    }

    #[test]
    fn cooldown_deduplicates_repeated_events() {
        let engine = RoutineEngine::default();
        assert!(engine.dispatch(service_event()).unwrap()[0].matched);
        let second = engine.dispatch(service_event()).unwrap();
        assert!(!second[0].matched);
        assert!(second[0].reason.contains("cooldown") || second[0].reason.contains("duplicate"));
    }
}
