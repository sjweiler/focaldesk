use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::planner::Planner;
use crate::provider::AiProvider;
use crate::types::{ChatMessage, ChatRequest, TokenUsage};

const MAX_TOOL_RESULT_CHARS: usize = 16_000;
const MAX_AGENT_CONTEXT_CHARS: usize = 48_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    WaitingForPermission,
    Queued,
    Running,
    AwaitingConfirmation,
    Completed,
    Failed,
    Cancelled,
}

impl AgentRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForPermission => "waiting_for_permission",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// A bounded, inspectable execution record owned by the AI service rather
/// than by the model or an individual client connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunStatus {
    pub run_id: String,
    pub agent_id: String,
    pub state: AgentRunState,
    pub objective_preview: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<AgentTriggerSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub created_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<u64>,
    pub deadline_at_unix: u64,
    pub max_tool_steps: usize,
    #[serde(default = "default_agent_context_chars")]
    pub max_context_chars: usize,
    #[serde(default = "default_agent_output_tokens")]
    pub max_output_tokens: u32,
    pub completed_tool_steps: usize,
    #[serde(default)]
    pub observations: Vec<AgentStepResult>,
    #[serde(default)]
    pub events: Vec<AgentRunEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<AgentResponse>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTriggerSource {
    pub trigger_id: String,
    pub kind: crate::agent_definition::AgentTriggerKind,
}

const fn default_agent_context_chars() -> usize {
    48_000
}

const fn default_agent_output_tokens() -> u32 {
    1_024
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunEvent {
    pub sequence: u64,
    pub at_unix: u64,
    #[serde(flatten)]
    pub kind: AgentRunEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentRunEventKind {
    Registered,
    Triggered {
        trigger_id: String,
        trigger_kind: crate::agent_definition::AgentTriggerKind,
    },
    Retried {
        source_run_id: String,
        recovered_steps: usize,
    },
    PermissionRequested,
    Queued,
    Planning {
        iteration: usize,
    },
    ToolStarted {
        step: usize,
        tool: String,
    },
    ToolCompleted {
        step: usize,
        tool: String,
    },
    ActionProposed {
        tool: String,
    },
    AwaitingConfirmation,
    Completed,
    Cancelled,
    Failed {
        message: String,
    },
}

pub trait AgentEventSink: Send + Sync {
    fn emit(&self, event: AgentRunEventKind);

    fn checkpoint_step(&self, _step: &AgentStepResult) {}
}

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
    pub agent_id: Option<String>,
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
    /// Assigned by the service runtime. Direct `Agent::run` callers leave it
    /// empty because they do not participate in the managed run lifecycle.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDryRunReport {
    pub agent_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub objective: String,
    pub permitted_tools: Vec<AgentToolSpec>,
    pub planned_steps: Vec<crate::planner::PlanStep>,
    pub answer: Option<String>,
    pub usage: Option<TokenUsage>,
    pub max_tool_steps: usize,
    pub max_context_chars: usize,
    pub max_output_tokens: u32,
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
        self.run_with_definition(provider, executor, request, None)
            .await
    }

    /// Asks the provider for one bounded plan but never invokes a tool.
    pub async fn dry_run_with_definition(
        &self,
        provider: &dyn AiProvider,
        executor: &dyn AgentToolExecutor,
        request: AgentRequest,
        definition: &crate::AgentDefinition,
    ) -> Result<AgentDryRunReport> {
        let mut tools = executor.tools();
        tools.retain(|tool| definition.tool_allowlist.contains(&tool.name));
        if tools.is_empty() {
            bail!("no agent tools are available");
        }
        let planning = provider
            .chat(ChatRequest {
                provider: request.provider.clone(),
                model: request.model.clone(),
                messages: vec![
                    ChatMessage::system(Planner::system_prompt_for(
                        &tools,
                        definition.max_tool_steps,
                        &definition.instructions,
                    )?),
                    ChatMessage::user(request.objective.clone()),
                ],
                temperature: Some(0.0),
                max_tokens: Some(definition.max_output_tokens),
                use_memory: false,
            })
            .await
            .context("agent dry-run planning request failed")?;
        let plan = Planner::parse_with_limit(&planning.content, &tools, definition.max_tool_steps)?;
        Ok(AgentDryRunReport {
            agent_id: definition.id.clone(),
            provider: planning.provider,
            model: planning.model,
            objective: request.objective,
            permitted_tools: tools,
            planned_steps: plan.steps,
            answer: plan.answer,
            usage: planning.usage,
            max_tool_steps: definition.max_tool_steps,
            max_context_chars: definition.max_context_chars,
            max_output_tokens: definition.max_output_tokens,
        })
    }

    pub async fn run_with_definition(
        &self,
        provider: &dyn AiProvider,
        executor: &dyn AgentToolExecutor,
        request: AgentRequest,
        definition: Option<&crate::AgentDefinition>,
    ) -> Result<AgentResponse> {
        let mut tools = executor.tools().into_iter().collect::<Vec<_>>();
        let (max_steps, max_output_tokens, instructions) = if let Some(definition) = definition {
            tools.retain(|tool| definition.tool_allowlist.contains(&tool.name));
            (
                definition.max_tool_steps,
                definition.max_output_tokens,
                definition.instructions.as_str(),
            )
        } else {
            (crate::planner::MAX_AGENT_STEPS, 1_024, "")
        };
        if tools.is_empty() {
            bail!("no agent tools are available");
        }

        let planning = provider
            .chat(ChatRequest {
                provider: request.provider.clone(),
                model: request.model.clone(),
                messages: vec![
                    ChatMessage::system(Planner::system_prompt_for(
                        &tools,
                        max_steps,
                        instructions,
                    )?),
                    ChatMessage::user(request.objective.clone()),
                ],
                temperature: Some(0.0),
                max_tokens: Some(max_output_tokens),
                use_memory: false,
            })
            .await
            .context("agent planning request failed")?;
        let mut usage = planning.usage;
        let plan = Planner::parse_with_limit(&planning.content, &tools, max_steps)?;

        if plan.steps.is_empty() {
            return Ok(AgentResponse {
                run_id: String::new(),
                provider: planning.provider,
                model: planning.model,
                answer: plan.answer.unwrap_or_default(),
                usage,
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
                max_tokens: Some(max_output_tokens),
                use_memory: false,
            })
            .await
            .context("agent synthesis request failed")?;
        merge_usage(&mut usage, synthesis.usage);

        Ok(AgentResponse {
            run_id: String::new(),
            provider: synthesis.provider,
            model: synthesis.model,
            answer: synthesis.content,
            usage,
            steps: results,
            proposed_action,
            confirmation: None,
        })
    }

    /// Runs a bounded observe-and-replan loop. Each planning call may select
    /// only one tool, so every subsequent decision sees the prior observation.
    pub async fn run_iterative_with_definition(
        &self,
        provider: &dyn AiProvider,
        executor: &dyn AgentToolExecutor,
        request: AgentRequest,
        definition: Option<&crate::AgentDefinition>,
        events: Option<&dyn AgentEventSink>,
    ) -> Result<AgentResponse> {
        self.run_iterative_from_checkpoint(
            provider,
            executor,
            request,
            definition,
            events,
            Vec::new(),
        )
        .await
    }

    pub async fn run_iterative_from_checkpoint(
        &self,
        provider: &dyn AiProvider,
        executor: &dyn AgentToolExecutor,
        request: AgentRequest,
        definition: Option<&crate::AgentDefinition>,
        events: Option<&dyn AgentEventSink>,
        initial_results: Vec<AgentStepResult>,
    ) -> Result<AgentResponse> {
        let mut tools = executor.tools();
        let (max_steps, max_context_chars, max_output_tokens, instructions) =
            if let Some(definition) = definition {
                tools.retain(|tool| definition.tool_allowlist.contains(&tool.name));
                (
                    definition.max_tool_steps,
                    definition.max_context_chars,
                    definition.max_output_tokens,
                    definition.instructions.as_str(),
                )
            } else {
                (
                    crate::planner::MAX_AGENT_STEPS,
                    MAX_AGENT_CONTEXT_CHARS,
                    1_024,
                    "",
                )
            };
        if tools.is_empty() {
            bail!("no agent tools are available");
        }

        if initial_results.len() > max_steps {
            bail!("agent checkpoint exceeds the configured tool-step budget");
        }
        let mut context = AgentContext::new(
            request.objective.clone(),
            initial_results,
            max_context_chars,
        );
        let mut provider_name = request.provider.clone().unwrap_or_default();
        let mut model = request.model.clone();
        let mut proposed_action = None;
        let mut direct_answer = None;
        let mut usage = None;

        for iteration in context.results.len() + 1..=max_steps {
            emit(events, AgentRunEventKind::Planning { iteration });
            let planning = provider
                .chat(ChatRequest {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                    messages: vec![
                        ChatMessage::system(Planner::system_prompt_for(
                            &tools,
                            1,
                            &format!(
                                "{instructions} Choose only the single next best tool after considering the supplied observations."
                            ),
                        )?),
                        ChatMessage::user(context.planning_prompt()),
                    ],
                    temperature: Some(0.0),
                    max_tokens: Some(max_output_tokens),
                    use_memory: false,
                })
                .await
                .context("agent planning request failed")?;
            merge_usage(&mut usage, planning.usage);
            provider_name = planning.provider;
            model = planning.model;
            let plan = Planner::parse_with_limit(&planning.content, &tools, 1)?;
            let Some(step) = plan.steps.into_iter().next() else {
                direct_answer = plan.answer;
                break;
            };
            let tool = tools
                .iter()
                .find(|tool| tool.name == step.tool)
                .expect("planner validated tool catalog membership");
            let step_number = context.results.len() + 1;
            if tool.mutating {
                emit(
                    events,
                    AgentRunEventKind::ActionProposed {
                        tool: step.tool.clone(),
                    },
                );
                proposed_action = Some(AgentProposedAction {
                    tool: step.tool,
                    arguments: step.arguments,
                });
                break;
            }

            emit(
                events,
                AgentRunEventKind::ToolStarted {
                    step: step_number,
                    tool: step.tool.clone(),
                },
            );
            let result = executor
                .execute(&step.tool, step.arguments.clone())
                .await
                .with_context(|| format!("agent tool {} failed", step.tool))?;
            let step_result = AgentStepResult {
                tool: step.tool.clone(),
                arguments: step.arguments,
                result: bound_value(result),
            };
            if let Some(events) = events {
                events.checkpoint_step(&step_result);
            }
            context.push(step_result);
            emit(
                events,
                AgentRunEventKind::ToolCompleted {
                    step: step_number,
                    tool: step.tool,
                },
            );
        }

        let results = context.results;
        if proposed_action.is_none()
            && let Some(answer) = direct_answer.filter(|answer| !answer.trim().is_empty())
        {
            return Ok(AgentResponse {
                run_id: String::new(),
                provider: provider_name,
                model,
                answer,
                usage,
                steps: results,
                proposed_action: None,
                confirmation: None,
            });
        }

        let evidence = bounded_json(&results, max_context_chars)?;
        let proposal =
            serde_json::to_string(&proposed_action).context("serialize proposed agent action")?;
        let synthesis = provider
            .chat(ChatRequest {
                provider: request.provider,
                model: request.model,
                messages: vec![
                    ChatMessage::system(
                        "Answer the user's objective using only the supplied FocalDesk observations. If a proposed action is present, explain that it is awaiting explicit user confirmation and has not executed. Be concise and state when the evidence is insufficient.",
                    ),
                    ChatMessage::user(format!(
                        "Objective: {}\nObservations: {evidence}\nProposed action: {proposal}",
                        request.objective
                    )),
                ],
                temperature: Some(0.0),
                max_tokens: Some(max_output_tokens),
                use_memory: false,
            })
            .await
            .context("agent synthesis request failed")?;
        merge_usage(&mut usage, synthesis.usage);

        Ok(AgentResponse {
            run_id: String::new(),
            provider: synthesis.provider,
            model: synthesis.model,
            answer: synthesis.content,
            usage,
            steps: results,
            proposed_action,
            confirmation: None,
        })
    }
}

fn merge_usage(total: &mut Option<TokenUsage>, additional: Option<TokenUsage>) {
    let Some(additional) = additional else {
        return;
    };
    let total = total.get_or_insert_with(TokenUsage::default);
    total.input_tokens = total.input_tokens.saturating_add(additional.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(additional.output_tokens);
}

fn emit(events: Option<&dyn AgentEventSink>, event: AgentRunEventKind) {
    if let Some(events) = events {
        events.emit(event);
    }
}

struct AgentContext {
    objective: String,
    results: Vec<AgentStepResult>,
    max_chars: usize,
}

impl AgentContext {
    fn new(objective: String, results: Vec<AgentStepResult>, max_chars: usize) -> Self {
        Self {
            objective,
            results,
            max_chars,
        }
    }

    fn push(&mut self, result: AgentStepResult) {
        self.results.push(result);
    }

    fn planning_prompt(&self) -> String {
        let observations =
            bounded_json(&self.results, self.max_chars).unwrap_or_else(|_| "[]".into());
        format!(
            "Objective: {}\nPrior observations: {observations}\nChoose the next tool, or return the final answer if the objective is resolved.",
            self.objective
        )
    }
}

fn bounded_json<T: Serialize>(value: &T, max_chars: usize) -> Result<String> {
    let encoded = serde_json::to_string(value).context("serialize bounded agent context")?;
    if encoded.chars().count() <= max_chars {
        return Ok(encoded);
    }
    Ok(format!(
        "{}…[older context truncated]",
        encoded
            .chars()
            .rev()
            .take(max_chars)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    ))
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

    struct RecordingProvider {
        responses: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<ChatRequest>>,
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
                citations: Vec::new(),
            })
        }
    }

    #[async_trait]
    impl AiProvider for RecordingProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "test".into(),
                kind: "test".into(),
                base_url: None,
                default_model: Some("test-model".into()),
            }
        }

        async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
            Ok(Vec::new())
        }

        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            self.requests.lock().unwrap().push(request);
            Ok(ChatResponse {
                provider: "test".into(),
                model: Some("test-model".into()),
                content: self.responses.lock().unwrap().pop_front().unwrap(),
                usage: Some(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 2,
                }),
                citations: Vec::new(),
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
                    agent_id: None,
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
                    agent_id: None,
                    provider: Some("test".into()),
                    model: None,
                },
            )
            .await
            .unwrap();
        assert!(tools.calls.lock().unwrap().is_empty());
        assert_eq!(response.proposed_action.unwrap().tool, "focus_window");
    }

    #[tokio::test]
    async fn iterative_agent_replans_from_the_previous_observation() {
        let provider = RecordingProvider {
            responses: Mutex::new(VecDeque::from([
                r#"{"steps":[{"tool":"list_windows","arguments":{}}],"answer":null}"#.into(),
                r#"{"steps":[],"answer":"The Editor window is open."}"#.into(),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let tools = TestTools::default();
        let response = Agent::new("test".into())
            .run_iterative_with_definition(
                &provider,
                &tools,
                AgentRequest {
                    objective: "What is open?".into(),
                    agent_id: None,
                    provider: Some("test".into()),
                    model: None,
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(response.answer, "The Editor window is open.");
        assert_eq!(response.steps.len(), 1);
        let usage = response.usage.unwrap();
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 4);
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages[1].content.contains("Editor"));
        assert!(
            requests
                .iter()
                .all(|request| request.messages[0].content.contains("no more than 1 steps"))
        );
    }

    #[tokio::test]
    async fn dry_run_returns_a_plan_without_executing_tools() {
        let provider = ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                r#"{"steps":[{"tool":"focus_window","arguments":{"id":7}}],"answer":null}"#.into(),
            ])),
        };
        let tools = TestTools::default();
        let definition = crate::built_in_agents().remove(0);
        let report = Agent::new("test".into())
            .dry_run_with_definition(
                &provider,
                &tools,
                AgentRequest {
                    objective: "Focus the editor".into(),
                    agent_id: Some("desktop".into()),
                    provider: Some("test".into()),
                    model: None,
                },
                &definition,
            )
            .await
            .unwrap();

        assert_eq!(report.planned_steps.len(), 1);
        assert_eq!(report.planned_steps[0].tool, "focus_window");
        assert!(tools.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn iterative_agent_resumes_from_a_safe_observation_checkpoint() {
        let provider = RecordingProvider {
            responses: Mutex::new(VecDeque::from([
                r#"{"steps":[],"answer":"No windows are open."}"#.into(),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let tools = TestTools::default();
        let response = Agent::new("test".into())
            .run_iterative_from_checkpoint(
                &provider,
                &tools,
                AgentRequest {
                    objective: "What is open?".into(),
                    agent_id: None,
                    provider: Some("test".into()),
                    model: None,
                },
                None,
                None,
                vec![AgentStepResult {
                    tool: "list_windows".into(),
                    arguments: json!({}),
                    result: json!({"windows": []}),
                }],
            )
            .await
            .unwrap();

        assert!(tools.calls.lock().unwrap().is_empty());
        assert_eq!(response.steps.len(), 1);
        assert!(
            provider.requests.lock().unwrap()[0].messages[1]
                .content
                .contains("windows")
        );
    }

    #[tokio::test]
    async fn iterative_agent_applies_manifest_model_output_budget() {
        let provider = RecordingProvider {
            responses: Mutex::new(VecDeque::from([r#"{"steps":[],"answer":"Done."}"#.into()])),
            requests: Mutex::new(Vec::new()),
        };
        let definition = crate::AgentBuilder::new("budget-test", "Budget test")
            .allow_tool("list_windows")
            .max_output_tokens(256)
            .build()
            .unwrap();
        Agent::new("test".into())
            .run_iterative_with_definition(
                &provider,
                &TestTools::default(),
                AgentRequest {
                    objective: "Check the budget".into(),
                    agent_id: None,
                    provider: Some("test".into()),
                    model: None,
                },
                Some(&definition),
                None,
            )
            .await
            .unwrap();

        assert_eq!(provider.requests.lock().unwrap()[0].max_tokens, Some(256));
    }
}
