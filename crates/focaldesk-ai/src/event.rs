use anyhow::{Result, anyhow, bail};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EVENTS: usize = 256;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Desktop,
    Calendar,
    Notification,
    ServiceHealth,
    Workflow,
}

impl EventSource {
    pub fn routine_kind(self) -> crate::RoutineEventKind {
        match self {
            Self::Desktop => crate::RoutineEventKind::Desktop,
            Self::Calendar => crate::RoutineEventKind::Calendar,
            Self::Notification => crate::RoutineEventKind::Notification,
            Self::ServiceHealth => crate::RoutineEventKind::Service,
            Self::Workflow => crate::RoutineEventKind::Workflow,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Calendar => "calendar",
            Self::Notification => "notification",
            Self::ServiceHealth => "service_health",
            Self::Workflow => "workflow",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSourceDescriptor {
    pub source: EventSource,
    pub supported_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSourcePolicy {
    pub source: EventSource,
    pub enabled: bool,
    pub allowed_fields: Vec<String>,
    pub retention_seconds: u64,
    pub forward_to_attention: bool,
}

impl EventSourcePolicy {
    pub fn validate(&mut self) -> Result<()> {
        if !(60..=86_400).contains(&self.retention_seconds) {
            bail!("event retention must be between 60 and 86400 seconds");
        }
        self.allowed_fields.sort();
        self.allowed_fields.dedup();
        let supported = supported_event_fields(self.source);
        if self.allowed_fields.len() > supported.len()
            || self
                .allowed_fields
                .iter()
                .any(|field| !supported.contains(&field.as_str()))
        {
            bail!("event policy contains unsupported fields");
        }
        if self.enabled && self.allowed_fields.is_empty() {
            bail!("an enabled event source requires at least one disclosed field");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub id: String,
    pub source: EventSource,
    pub producer: String,
    pub payload: Value,
    pub summary: String,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
}

impl EventEnvelope {
    pub fn routine_event(&self) -> crate::RoutineEvent {
        crate::RoutineEvent {
            kind: self.source.routine_kind(),
            value: self.summary.clone(),
            source: format!("{} via {}", self.source.as_str(), self.producer),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventDelivery {
    pub event: EventEnvelope,
    pub evaluations: Vec<crate::RoutineEvaluation>,
    pub simulated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventFabricSnapshot {
    pub connected: bool,
    pub descriptors: Vec<EventSourceDescriptor>,
    pub policies: Vec<EventSourcePolicy>,
    pub events: Vec<EventEnvelope>,
}

struct EventFabricState {
    connected: bool,
    policies: BTreeMap<EventSource, EventSourcePolicy>,
    events: BTreeMap<String, EventEnvelope>,
}

pub struct EventFabric {
    state: Mutex<EventFabricState>,
}

impl Default for EventFabric {
    fn default() -> Self {
        let policies = all_sources()
            .into_iter()
            .map(|source| {
                (
                    source,
                    EventSourcePolicy {
                        source,
                        enabled: false,
                        allowed_fields: Vec::new(),
                        retention_seconds: 3_600,
                        forward_to_attention: true,
                    },
                )
            })
            .collect();
        Self {
            state: Mutex::new(EventFabricState {
                connected: true,
                policies,
                events: BTreeMap::new(),
            }),
        }
    }
}

impl EventFabric {
    pub fn snapshot(&self) -> Result<EventFabricSnapshot> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        prune(&mut state, now);
        Ok(EventFabricSnapshot {
            connected: state.connected,
            descriptors: all_sources()
                .into_iter()
                .map(|source| EventSourceDescriptor {
                    source,
                    supported_fields: supported_event_fields(source)
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                })
                .collect(),
            policies: state.policies.values().cloned().collect(),
            events: state.events.values().cloned().collect(),
        })
    }

    pub fn configure(&self, mut policy: EventSourcePolicy) -> Result<EventSourcePolicy> {
        policy.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        state.policies.insert(policy.source, policy.clone());
        Ok(policy)
    }

    pub fn set_connected(&self, connected: bool) -> Result<bool> {
        self.state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?
            .connected = connected;
        Ok(connected)
    }

    pub fn ingest(
        &self,
        source: EventSource,
        producer: String,
        payload: Value,
    ) -> Result<(EventEnvelope, bool)> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        prune(&mut state, now);
        let (event, forward) = prepare_event(&state, source, producer, payload, now)?;
        while state.events.len() >= MAX_EVENTS {
            let Some(oldest) = state
                .events
                .values()
                .min_by_key(|item| item.created_at_unix)
                .map(|item| item.id.clone())
            else {
                break;
            };
            state.events.remove(&oldest);
        }
        state.events.insert(event.id.clone(), event.clone());
        Ok((event, forward))
    }

    pub fn simulate(
        &self,
        source: EventSource,
        producer: String,
        payload: Value,
    ) -> Result<(EventEnvelope, bool)> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        prepare_event(&state, source, producer, payload, unix_now())
    }

    pub fn event(&self, event_id: &str) -> Result<EventEnvelope> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        prune(&mut state, now);
        state
            .events
            .get(event_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown or expired event"))
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("event fabric unavailable"))?;
        let count = state.events.len();
        state.events.clear();
        Ok(count)
    }
}

fn prepare_event(
    state: &EventFabricState,
    source: EventSource,
    producer: String,
    payload: Value,
    now: u64,
) -> Result<(EventEnvelope, bool)> {
    if !state.connected {
        bail!("event fabric is disconnected by the emergency switch");
    }
    if producer.trim().is_empty() || producer.chars().count() > 200 {
        bail!("event producer must contain 1-200 characters");
    }
    if serde_json::to_vec(&payload)?.len() > MAX_PAYLOAD_BYTES {
        bail!("event payload exceeds 16 KiB");
    }
    let policy = state
        .policies
        .get(&source)
        .ok_or_else(|| anyhow!("unknown event source"))?;
    if !policy.enabled {
        bail!("event source is disabled; enable its disclosure policy first");
    }
    let object = payload
        .as_object()
        .ok_or_else(|| anyhow!("event payload must be a JSON object"))?;
    let mut disclosed = Map::new();
    for field in &policy.allowed_fields {
        if let Some(value) = object.get(field) {
            if !is_scalar(value) {
                bail!("event field '{field}' must be a scalar value");
            }
            disclosed.insert(field.clone(), value.clone());
        }
    }
    if disclosed.is_empty() {
        bail!("event contains none of the source's disclosed fields");
    }
    let summary = disclosed
        .iter()
        .map(|(key, value)| format!("{key}={}", scalar_text(value)))
        .collect::<Vec<_>>()
        .join("; ");
    let event = EventEnvelope {
        id: random_id("event"),
        source,
        producer,
        payload: Value::Object(disclosed),
        summary: summary.chars().take(2_000).collect(),
        created_at_unix: now,
        expires_at_unix: now.saturating_add(policy.retention_seconds),
    };
    Ok((event, policy.forward_to_attention))
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn all_sources() -> [EventSource; 5] {
    [
        EventSource::Desktop,
        EventSource::Calendar,
        EventSource::Notification,
        EventSource::ServiceHealth,
        EventSource::Workflow,
    ]
}

pub fn supported_event_fields(source: EventSource) -> Vec<&'static str> {
    match source {
        EventSource::Desktop => vec!["event", "app_id", "workspace_id", "window_title"],
        EventSource::Calendar => vec!["event", "title", "start_time", "end_time", "organizer"],
        EventSource::Notification => vec!["event", "app_id", "summary", "body", "urgency"],
        EventSource::ServiceHealth => {
            vec!["event", "service", "state", "message", "failure_count"]
        }
        EventSource::Workflow => vec!["event", "workflow_id", "run_id", "state", "error"],
    }
}

fn prune(state: &mut EventFabricState, now: u64) {
    state.events.retain(|_, event| event.expires_at_unix > now);
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
    use serde_json::json;

    fn enabled_policy() -> EventSourcePolicy {
        EventSourcePolicy {
            source: EventSource::ServiceHealth,
            enabled: true,
            allowed_fields: vec!["service".into(), "state".into()],
            retention_seconds: 300,
            forward_to_attention: true,
        }
    }

    #[test]
    fn sources_are_disabled_until_explicitly_configured() {
        let fabric = EventFabric::default();
        assert!(
            fabric
                .ingest(
                    EventSource::ServiceHealth,
                    "test".into(),
                    json!({"service":"renderer"}),
                )
                .is_err()
        );
    }

    #[test]
    fn journal_retains_only_allowlisted_scalar_fields() {
        let fabric = EventFabric::default();
        fabric.configure(enabled_policy()).unwrap();
        let (event, forward) = fabric
            .ingest(
                EventSource::ServiceHealth,
                "test".into(),
                json!({"service":"renderer", "state":"failed", "secret":"drop me"}),
            )
            .unwrap();
        assert!(forward);
        assert_eq!(
            event.payload,
            json!({"service":"renderer", "state":"failed"})
        );
        assert!(!event.summary.contains("secret"));
    }

    #[test]
    fn simulation_does_not_retain_an_event() {
        let fabric = EventFabric::default();
        fabric.configure(enabled_policy()).unwrap();
        fabric
            .simulate(
                EventSource::ServiceHealth,
                "test".into(),
                json!({"service":"renderer", "state":"failed"}),
            )
            .unwrap();
        assert!(fabric.snapshot().unwrap().events.is_empty());
    }

    #[test]
    fn emergency_disconnect_stops_intake() {
        let fabric = EventFabric::default();
        fabric.configure(enabled_policy()).unwrap();
        fabric.set_connected(false).unwrap();
        assert!(
            fabric
                .ingest(
                    EventSource::ServiceHealth,
                    "test".into(),
                    json!({"service":"renderer"}),
                )
                .is_err()
        );
    }
}
