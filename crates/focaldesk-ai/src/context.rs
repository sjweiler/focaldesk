use anyhow::{Result, anyhow, bail};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CONTEXT_ITEMS: usize = 128;
const MAX_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    ActiveWindow,
    Workspace,
    Notifications,
    Calendar,
    Files,
    Conversation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSensitivity {
    Public,
    Private,
    Restricted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextEnvelope {
    pub id: String,
    pub kind: ContextKind,
    pub provenance: String,
    pub sensitivity: ContextSensitivity,
    pub payload: Value,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextGrant {
    pub id: String,
    pub agent_id: String,
    pub kinds: Vec<ContextKind>,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextSuggestion {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub body: String,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub dismissed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentDestination {
    Chat,
    Agent { agent_id: String },
    Workflow { workflow_id: String },
    SuggestionInbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentRoute {
    pub destination: IntentDestination,
    pub required_context: Vec<ContextKind>,
    pub reason: String,
    pub ambiguous: bool,
}

#[derive(Default)]
struct ContextState {
    items: BTreeMap<String, ContextEnvelope>,
    grants: BTreeMap<String, ContextGrant>,
    suggestions: BTreeMap<String, ContextSuggestion>,
}

#[derive(Default)]
pub struct ContextBroker {
    state: Mutex<ContextState>,
}

impl ContextBroker {
    pub fn publish(
        &self,
        kind: ContextKind,
        provenance: String,
        sensitivity: ContextSensitivity,
        payload: Value,
        ttl_seconds: u64,
    ) -> Result<ContextEnvelope> {
        if !(5..=3_600).contains(&ttl_seconds) {
            bail!("context TTL must be between 5 and 3600 seconds");
        }
        if provenance.trim().is_empty() || provenance.len() > 200 {
            bail!("context provenance must contain 1-200 characters");
        }
        if serde_json::to_vec(&payload)?.len() > MAX_CONTEXT_BYTES {
            bail!("context payload exceeds 16 KiB");
        }
        let now = unix_now();
        let envelope = ContextEnvelope {
            id: random_id("ctx"),
            kind,
            provenance,
            sensitivity,
            payload,
            created_at_unix: now,
            expires_at_unix: now.saturating_add(ttl_seconds),
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        prune(&mut state, now);
        while state.items.len() >= MAX_CONTEXT_ITEMS {
            let Some(oldest) = state
                .items
                .values()
                .min_by_key(|item| item.created_at_unix)
                .map(|item| item.id.clone())
            else {
                break;
            };
            state.items.remove(&oldest);
        }
        state.items.insert(envelope.id.clone(), envelope.clone());
        Ok(envelope)
    }

    pub fn grant(
        &self,
        agent_id: String,
        mut kinds: Vec<ContextKind>,
        ttl_seconds: u64,
    ) -> Result<ContextGrant> {
        if agent_id.trim().is_empty() || agent_id.len() > 64 || !(10..=3_600).contains(&ttl_seconds)
        {
            bail!("context grant is out of bounds");
        }
        kinds.sort();
        kinds.dedup();
        if kinds.is_empty() || kinds.len() > 6 {
            bail!("context grant must contain 1-6 unique kinds");
        }
        let now = unix_now();
        let grant = ContextGrant {
            id: random_id("ctxg"),
            agent_id,
            kinds,
            issued_at_unix: now,
            expires_at_unix: now.saturating_add(ttl_seconds),
            revoked: false,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        prune(&mut state, now);
        state.grants.insert(grant.id.clone(), grant.clone());
        Ok(grant)
    }

    pub fn for_agent(
        &self,
        agent_id: &str,
        allowed: Option<&[ContextKind]>,
    ) -> Result<Vec<ContextEnvelope>> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        prune(&mut state, now);
        let granted = state
            .grants
            .values()
            .filter(|grant| !grant.revoked && grant.agent_id == agent_id)
            .flat_map(|grant| grant.kinds.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut items = state
            .items
            .values()
            .filter(|item| {
                granted.contains(&item.kind)
                    && allowed.is_none_or(|allowed| allowed.contains(&item.kind))
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.created_at_unix);
        Ok(items)
    }

    pub fn snapshot(
        &self,
    ) -> Result<(
        Vec<ContextEnvelope>,
        Vec<ContextGrant>,
        Vec<ContextSuggestion>,
    )> {
        let now = unix_now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        prune(&mut state, now);
        Ok((
            state.items.values().cloned().collect(),
            state.grants.values().cloned().collect(),
            state.suggestions.values().cloned().collect(),
        ))
    }

    pub fn revoke(&self, grant_id: &str) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        let Some(grant) = state.grants.get_mut(grant_id) else {
            return Ok(false);
        };
        grant.revoked = true;
        Ok(true)
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        let count = state.items.len();
        state.items.clear();
        Ok(count)
    }

    pub fn suggest(
        &self,
        agent_id: String,
        title: String,
        body: String,
        ttl_seconds: u64,
    ) -> Result<ContextSuggestion> {
        if agent_id.trim().is_empty()
            || title.trim().is_empty()
            || title.chars().count() > 120
            || body.trim().is_empty()
            || body.chars().count() > 2_000
            || !(30..=86_400).contains(&ttl_seconds)
        {
            bail!("suggestion is out of bounds");
        }
        let now = unix_now();
        let suggestion = ContextSuggestion {
            id: random_id("suggestion"),
            agent_id,
            title,
            body,
            created_at_unix: now,
            expires_at_unix: now.saturating_add(ttl_seconds),
            dismissed: false,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        prune(&mut state, now);
        if state.suggestions.len() >= 64
            && let Some(oldest) = state
                .suggestions
                .values()
                .min_by_key(|item| item.created_at_unix)
                .map(|item| item.id.clone())
        {
            state.suggestions.remove(&oldest);
        }
        state
            .suggestions
            .insert(suggestion.id.clone(), suggestion.clone());
        Ok(suggestion)
    }

    pub fn dismiss_suggestion(&self, suggestion_id: &str) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("context broker unavailable"))?;
        let Some(suggestion) = state.suggestions.get_mut(suggestion_id) else {
            return Ok(false);
        };
        suggestion.dismissed = true;
        Ok(true)
    }
}

pub fn route_intent(text: &str) -> IntentRoute {
    let text = text.trim().to_ascii_lowercase();
    if text.contains("next meeting") || text.contains("prepare for my meeting") {
        IntentRoute {
            destination: IntentDestination::Workflow {
                workflow_id: "meeting-preparation".into(),
            },
            required_context: vec![ContextKind::Calendar],
            reason: "meeting preparation phrase".into(),
            ambiguous: false,
        }
    } else if text.contains("fix what")
        || text.contains("what's wrong here")
        || text.contains("what is wrong here")
    {
        IntentRoute {
            destination: IntentDestination::Workflow {
                workflow_id: "workspace-troubleshooter".into(),
            },
            required_context: vec![ContextKind::ActiveWindow, ContextKind::Workspace],
            reason: "workspace troubleshooting phrase".into(),
            ambiguous: false,
        }
    } else if text.contains("summarize this") || text.contains("explain this") {
        IntentRoute {
            destination: IntentDestination::Agent {
                agent_id: "accessibility".into(),
            },
            required_context: vec![ContextKind::ActiveWindow, ContextKind::Conversation],
            reason: "current-context explanation phrase".into(),
            ambiguous: false,
        }
    } else {
        IntentRoute {
            destination: IntentDestination::Chat,
            required_context: Vec::new(),
            reason: "no deterministic route matched".into(),
            ambiguous: true,
        }
    }
}

fn prune(state: &mut ContextState, now: u64) {
    state.items.retain(|_, item| item.expires_at_unix > now);
    state.grants.retain(|_, grant| grant.expires_at_unix > now);
    state
        .suggestions
        .retain(|_, suggestion| suggestion.expires_at_unix > now);
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

    #[test]
    fn context_requires_an_explicit_agent_grant() {
        let broker = ContextBroker::default();
        broker
            .publish(
                ContextKind::ActiveWindow,
                "desktop snapshot".into(),
                ContextSensitivity::Private,
                json!({"title":"Editor"}),
                60,
            )
            .unwrap();
        assert!(broker.for_agent("desktop", None).unwrap().is_empty());
        broker
            .grant("desktop".into(), vec![ContextKind::ActiveWindow], 60)
            .unwrap();
        assert_eq!(broker.for_agent("desktop", None).unwrap().len(), 1);
        assert!(broker.for_agent("desktop", Some(&[])).unwrap().is_empty());
    }

    #[test]
    fn deterministic_routes_are_bounded_and_explainable() {
        let route = route_intent("prepare for my next meeting");
        assert_eq!(route.required_context, vec![ContextKind::Calendar]);
        assert!(!route.ambiguous);
        assert!(matches!(
            route.destination,
            IntentDestination::Workflow { .. }
        ));
    }

    #[test]
    fn suggestions_are_inert_until_explicitly_dismissed() {
        let broker = ContextBroker::default();
        let suggestion = broker
            .suggest(
                "desktop".into(),
                "Review updates".into(),
                "Three updates are available.".into(),
                300,
            )
            .unwrap();
        assert!(!suggestion.dismissed);
        assert!(broker.dismiss_suggestion(&suggestion.id).unwrap());
        let (_, _, suggestions) = broker.snapshot().unwrap();
        assert!(suggestions[0].dismissed);
    }
}
