use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::planner::Planner;
use crate::provider::AiProvider;
use crate::types::{ChatMessage, ChatRequest};

const MAX_TOOL_RESULT_CHARS: usize = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub mutating: bool,
}

#[async_trait]
pub trait AgentToolExecutor: Send + Sync {
    fn tools(&self) -> Vec<AgentToolSpec>;
    async fn execute(&self, tool: &str, arguments: Value) -> Result<Value>;

    async fn execute_confirmed(&self, _tool: &str, _arguments: Value) -> Result<Value> {
        bail!("confirmed agent actions are not supported by this executor")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub objective: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStepResult {
    pub tool: String,
    pub arguments: Value,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProposedAction {
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfirmation {
    pub plan_id: String,
    pub expires_at_unix: u64,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub provider: String,
    pub model: Option<String>,
    pub answer: String,
    pub steps: Vec<AgentStepResult>,
    #[serde(default)]
    pub proposed_action: Option<AgentProposedAction>,
    #[serde(default)]
    pub confirmation: Option<AgentConfirmation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActionResponse {
    pub plan_id: String,
    pub tool: String,
    pub executed: bool,
    #[serde(default)]
    pub result: Option<Value>,
}

#[derive(Debug, Default)]
pub struct Agent {
    pub name: String,
}

impl Agent {
    pub fn new(agent_name: String) -> Self {
        Self { name: agent_name }
    }

    pub async fn run(
        &self,
        provider: &dyn AiProvider,
        executor: &dyn AgentToolExecutor,
        request: AgentRequest,
    ) -> Result<AgentResponse> {
        let tools = executor.tools().into_iter().collect::<Vec<_>>();
        if tools.is_empty() {
            bail!("no agent tools are available");
        }

        let planning = provider
            .chat(ChatRequest {
                provider: request.provider.clone(),
                model: request.model.clone(),
                messages: vec![
                    ChatMessage::system(Planner::system_prompt(&tools)?),
                    ChatMessage::user(request.objective.clone()),
                ],
                temperature: Some(0.0),
                max_tokens: Some(1_024),
                use_memory: false,
            })
            .await
            .context("agent planning request failed")?;
        let plan = Planner::parse(&planning.content, &tools)?;

        if plan.steps.is_empty() {
            return Ok(AgentResponse {
                provider: planning.provider,
                model: planning.model,
                answer: plan.answer.unwrap_or_default(),
                steps: Vec::new(),
                proposed_action: None,
                confirmation: None,
            });
        }

        let mut results = Vec::with_capacity(plan.steps.len());
        let mut proposed_action = None;
        for step in plan.steps {
            let tool = tools
                .iter()
                .find(|tool| tool.name == step.tool)
                .expect("planner validated tool catalog membership");
            if tool.mutating {
                proposed_action = Some(AgentProposedAction {
                    tool: step.tool,
                    arguments: step.arguments,
                });
                break;
            }
            let result = executor
                .execute(&step.tool, step.arguments.clone())
                .await
                .with_context(|| format!("agent tool {} failed", step.tool))?;
            results.push(AgentStepResult {
                tool: step.tool,
                arguments: step.arguments,
                result: bound_value(result),
            });
        }

        let evidence = serde_json::to_string(&results).context("serialize agent tool results")?;
        let proposal =
            serde_json::to_string(&proposed_action).context("serialize proposed agent action")?;
        let synthesis = provider
            .chat(ChatRequest {
                provider: request.provider,
                model: request.model,
                messages: vec![
                    ChatMessage::system(
                        "Answer the user's objective using only the supplied FocalDesk tool results. If a proposed action is present, explain that it is awaiting explicit user confirmation and has not executed. Be concise and state when the evidence is insufficient.",
                    ),
                    ChatMessage::user(format!(
                        "Objective: {}\nTool results: {evidence}\nProposed action: {proposal}",
                        request.objective
                    )),
                ],
                temperature: Some(0.0),
                max_tokens: Some(1_024),
                use_memory: false,
            })
            .await
            .context("agent synthesis request failed")?;

        Ok(AgentResponse {
            provider: synthesis.provider,
            model: synthesis.model,
            answer: synthesis.content,
            steps: results,
            proposed_action,
            confirmation: None,
        })
    }
}

fn bound_value(value: Value) -> Value {
    let encoded = serde_json::to_string(&value).unwrap_or_default();
    if encoded.chars().count() <= MAX_TOOL_RESULT_CHARS {
        value
    } else {
        Value::String(format!(
            "{}…[tool result truncated]",
            encoded
                .chars()
                .take(MAX_TOOL_RESULT_CHARS)
                .collect::<String>()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatResponse, ProviderInfo, ProviderModelInfo};
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ScriptedProvider {
        responses: Mutex<VecDeque<String>>,
    }

    #[async_trait]
    impl AiProvider for ScriptedProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "test".into(),
                kind: "test".into(),
                base_url: None,
                default_model: Some("test-model".into()),
            }
        }

        async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
            Ok(vec![ProviderModelInfo {
                id: "test-model".into(),
            }])
        }

        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                provider: "test".into(),
                model: Some("test-model".into()),
                content: self.responses.lock().unwrap().pop_front().unwrap(),
                usage: None,
            })
        }
    }

    #[derive(Default)]
    struct TestTools {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl AgentToolExecutor for TestTools {
        fn tools(&self) -> Vec<AgentToolSpec> {
            vec![
                AgentToolSpec {
                    name: "list_windows".into(),
                    description: "List windows".into(),
                    input_schema: json!({"type": "object"}),
                    mutating: false,
                },
                AgentToolSpec {
                    name: "focus_window".into(),
                    description: "Focus a window".into(),
                    input_schema: json!({"type": "object"}),
                    mutating: true,
                },
            ]
        }

        async fn execute(&self, tool: &str, _arguments: Value) -> Result<Value> {
            self.calls.lock().unwrap().push(tool.to_string());
            Ok(json!({"windows": [{"id": 7, "title": "Editor"}]}))
        }
    }

    #[tokio::test]
    async fn agent_plans_executes_and_synthesizes_read_only_tools() {
        let provider = ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                r#"{"steps":[{"tool":"list_windows","arguments":{}}],"answer":null}"#.into(),
                "The Editor window is open.".into(),
            ])),
        };
        let tools = TestTools::default();
        let response = Agent::new("test".into())
            .run(
                &provider,
                &tools,
                AgentRequest {
                    objective: "What is open?".into(),
                    provider: Some("test".into()),
                    model: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(tools.calls.lock().unwrap().as_slice(), ["list_windows"]);
        assert_eq!(response.steps.len(), 1);
        assert_eq!(response.answer, "The Editor window is open.");
        assert!(response.proposed_action.is_none());
    }

    #[tokio::test]
    async fn mutation_tools_are_proposed_but_never_executed() {
        let provider = ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                r#"{"steps":[{"tool":"focus_window","arguments":{"window_id":7}}],"answer":null}"#
                    .into(),
                "Focusing the window is awaiting confirmation.".into(),
            ])),
        };
        let tools = TestTools::default();
        let response = Agent::new("test".into())
            .run(
                &provider,
                &tools,
                AgentRequest {
                    objective: "Focus the editor".into(),
                    provider: Some("test".into()),
                    model: None,
                },
            )
            .await
            .unwrap();
        assert!(tools.calls.lock().unwrap().is_empty());
        assert_eq!(response.proposed_action.unwrap().tool, "focus_window");
    }
}
