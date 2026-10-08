use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use crate::{
    AGENT_MANIFEST_VERSION, AgentDefinition, AgentToolExecutor, AgentToolSpec, AgentTrigger,
    AiProvider, ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo,
};

/// Fluent, validation-backed construction for declarative agent manifests.
#[derive(Debug, Clone)]
pub struct AgentBuilder {
    definition: AgentDefinition,
}

impl AgentBuilder {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            definition: AgentDefinition {
                manifest_version: AGENT_MANIFEST_VERSION,
                id: id.into(),
                name: name.into(),
                description: String::new(),
                instructions: String::new(),
                tool_allowlist: Vec::new(),
                max_tool_steps: 4,
                max_context_chars: 48_000,
                max_output_tokens: 1_024,
                timeout_seconds: 120,
                memory: false,
                voice: false,
                triggers: Vec::new(),
                daily_token_limit: None,
                daily_cost_limit_microusd: None,
                input_cost_microusd_per_million: None,
                output_cost_microusd_per_million: None,
                capability_policy: None,
                built_in: false,
            },
        }
    }

    pub fn description(mut self, value: impl Into<String>) -> Self {
        self.definition.description = value.into();
        self
    }

    pub fn instructions(mut self, value: impl Into<String>) -> Self {
        self.definition.instructions = value.into();
        self
    }

    pub fn allow_tool(mut self, tool: impl Into<String>) -> Self {
        self.definition.tool_allowlist.push(tool.into());
        self
    }

    pub fn allow_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.definition
            .tool_allowlist
            .extend(tools.into_iter().map(Into::into));
        self
    }

    pub fn max_tool_steps(mut self, value: usize) -> Self {
        self.definition.max_tool_steps = value;
        self
    }

    pub fn max_context_chars(mut self, value: usize) -> Self {
        self.definition.max_context_chars = value;
        self
    }

    pub fn max_output_tokens(mut self, value: u32) -> Self {
        self.definition.max_output_tokens = value;
        self
    }

    pub fn timeout_seconds(mut self, value: u64) -> Self {
        self.definition.timeout_seconds = value;
        self
    }

    pub fn memory(mut self, enabled: bool) -> Self {
        self.definition.memory = enabled;
        self
    }

    pub fn voice(mut self, enabled: bool) -> Self {
        self.definition.voice = enabled;
        self
    }

    pub fn trigger(mut self, trigger: AgentTrigger) -> Self {
        self.definition.triggers.push(trigger);
        self
    }

    pub fn daily_token_limit(mut self, value: Option<u64>) -> Self {
        self.definition.daily_token_limit = value;
        self
    }

    pub fn daily_cost_budget(
        mut self,
        limit_microusd: Option<u64>,
        input_microusd_per_million: Option<u64>,
        output_microusd_per_million: Option<u64>,
    ) -> Self {
        self.definition.daily_cost_limit_microusd = limit_microusd;
        self.definition.input_cost_microusd_per_million = input_microusd_per_million;
        self.definition.output_cost_microusd_per_million = output_microusd_per_million;
        self
    }

    pub fn capability_policy(mut self, policy: Option<crate::CapabilityPolicy>) -> Self {
        self.definition.capability_policy = policy;
        self
    }

    pub fn build(self) -> Result<AgentDefinition> {
        self.definition.validate()?;
        Ok(self.definition)
    }
}

/// Deterministic provider for SDK and package contract tests.
pub struct ScriptedAgentProvider {
    responses: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedAgentProvider {
    pub fn new(responses: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Into::into).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl AiProvider for ScriptedAgentProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "scripted-agent-sdk".into(),
            kind: "test".into(),
            base_url: None,
            default_model: Some("deterministic".into()),
        }
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        Ok(vec![ProviderModelInfo {
            id: "deterministic".into(),
        }])
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.requests
            .lock()
            .map_err(|_| anyhow!("scripted request log is unavailable"))?
            .push(request);
        let content = self
            .responses
            .lock()
            .map_err(|_| anyhow!("scripted response queue is unavailable"))?
            .pop_front()
            .ok_or_else(|| anyhow!("scripted agent response queue is exhausted"))?;
        Ok(ChatResponse {
            provider: "scripted-agent-sdk".into(),
            model: Some("deterministic".into()),
            content,
            usage: None,
            citations: Vec::new(),
        })
    }
}

/// Auditable mock tool executor with explicit results and a recorded call log.
pub struct MockAgentTools {
    tools: Vec<AgentToolSpec>,
    results: BTreeMap<String, Value>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl MockAgentTools {
    pub fn new(tools: Vec<AgentToolSpec>) -> Self {
        Self {
            tools,
            results: BTreeMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn with_result(mut self, tool: impl Into<String>, result: Value) -> Self {
        self.results.insert(tool.into(), result);
        self
    }

    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl AgentToolExecutor for MockAgentTools {
    fn tools(&self) -> Vec<AgentToolSpec> {
        self.tools.clone()
    }

    async fn execute(&self, tool: &str, arguments: Value) -> Result<Value> {
        self.calls
            .lock()
            .map_err(|_| anyhow!("mock tool call log is unavailable"))?
            .push((tool.to_string(), arguments));
        self.results
            .get(tool)
            .cloned()
            .ok_or_else(|| anyhow!("mock result is not configured for tool {tool}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentTriggerKind;

    #[test]
    fn builder_produces_a_round_trippable_bounded_manifest() {
        let definition = AgentBuilder::new("workspace-guide", "Workspace Guide")
            .description("Explains the active workspace")
            .instructions("Use observed window metadata only.")
            .allow_tools(["list_windows", "list_workspaces"])
            .max_tool_steps(2)
            .max_context_chars(16_000)
            .max_output_tokens(512)
            .timeout_seconds(45)
            .voice(true)
            .trigger(AgentTrigger {
                id: "on-login".into(),
                kind: AgentTriggerKind::DesktopEvent,
                objective: "Summarize the session after login.".into(),
                match_value: "session_started".into(),
                interval_seconds: None,
                cooldown_seconds: 300,
                max_runs_per_hour: 2,
                enabled: true,
            })
            .build()
            .unwrap();
        let encoded = definition.to_toml().unwrap();
        assert_eq!(AgentDefinition::from_toml(&encoded).unwrap(), definition);
    }

    #[test]
    fn builder_rejects_unbounded_or_authority_free_agents() {
        assert!(AgentBuilder::new("empty", "Empty").build().is_err());
        assert!(
            AgentBuilder::new("slow", "Slow")
                .allow_tool("list_windows")
                .timeout_seconds(121)
                .build()
                .is_err()
        );
    }
}
