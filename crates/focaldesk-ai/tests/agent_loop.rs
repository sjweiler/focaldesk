use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use focaldesk_ai::{
    Agent, AgentRequest, AgentToolExecutor, AgentToolSpec, AiProvider, ChatRequest, ChatResponse,
    ProviderInfo, ProviderModelInfo,
};
use serde_json::{Value, json};

struct ScriptedProvider {
    responses: Mutex<VecDeque<Result<String>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    fn with_responses(responses: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            responses: Mutex::new(
                responses
                    .into_iter()
                    .map(|response| Ok(response.to_string()))
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl AiProvider for ScriptedProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "scripted".into(),
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
        self.requests.lock().unwrap().push(request);
        let content = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("scripted provider response exhausted"))??;
        Ok(ChatResponse {
            provider: "scripted".into(),
            model: Some("deterministic".into()),
            content,
            usage: None,
        })
    }
}

#[derive(Default)]
struct RecordingTools {
    calls: Mutex<Vec<(String, Value)>>,
    fail_tool: Option<&'static str>,
    oversized_result: bool,
}

#[async_trait]
impl AgentToolExecutor for RecordingTools {
    fn tools(&self) -> Vec<AgentToolSpec> {
        vec![
            AgentToolSpec {
                name: "list_windows".into(),
                description: "List visible windows".into(),
                input_schema: json!({"type":"object","additionalProperties":false}),
                mutating: false,
            },
            AgentToolSpec {
                name: "inspect_workspace".into(),
                description: "Inspect one workspace".into(),
                input_schema: json!({"type":"object","properties":{"id":{"type":"integer"}}}),
                mutating: false,
            },
            AgentToolSpec {
                name: "focus_window".into(),
                description: "Focus one window".into(),
                input_schema: json!({"type":"object","properties":{"id":{"type":"integer"}}}),
                mutating: true,
            },
        ]
    }

    async fn execute(&self, tool: &str, arguments: Value) -> Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((tool.to_string(), arguments));
        if self.fail_tool == Some(tool) {
            return Err(anyhow!("deterministic tool failure"));
        }
        if self.oversized_result {
            return Ok(json!({"payload": "x".repeat(20_000)}));
        }
        match tool {
            "list_windows" => Ok(json!({"windows":[{"id":7,"title":"Editor"}]})),
            "inspect_workspace" => Ok(json!({"workspace":2,"window_count":1})),
            other => Err(anyhow!("unexpected read-only execution: {other}")),
        }
    }
}

fn request() -> AgentRequest {
    AgentRequest {
        objective: "Inspect the desktop".into(),
        provider: Some("scripted".into()),
        model: Some("deterministic".into()),
    }
}

#[tokio::test]
async fn agent_contract_runs_multiple_read_only_steps_then_synthesizes_evidence() {
    let provider = ScriptedProvider::with_responses([
        r#"{"steps":[{"tool":"list_windows","arguments":{}},{"tool":"inspect_workspace","arguments":{"id":2}}],"answer":null}"#,
        "The Editor is the only window on workspace 2.",
    ]);
    let tools = RecordingTools::default();

    let response = Agent::new("integration-agent".into())
        .run(&provider, &tools, request())
        .await
        .unwrap();

    assert_eq!(
        response.answer,
        "The Editor is the only window on workspace 2."
    );
    assert_eq!(response.steps.len(), 2);
    assert!(response.proposed_action.is_none());
    assert_eq!(
        tools
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(tool, _)| tool.as_str())
            .collect::<Vec<_>>(),
        ["list_windows", "inspect_workspace"]
    );

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].temperature, Some(0.0));
    assert_eq!(requests[0].max_tokens, Some(1_024));
    assert!(requests[0].messages[0].content.contains("list_windows"));
    assert!(
        requests[1].messages[1]
            .content
            .contains("\"title\":\"Editor\"")
    );
    assert!(requests[1].messages[1].content.contains("\"workspace\":2"));
}

#[tokio::test]
async fn mutation_is_only_proposed_after_read_only_evidence() {
    let provider = ScriptedProvider::with_responses([
        r#"{"steps":[{"tool":"list_windows","arguments":{}},{"tool":"focus_window","arguments":{"id":7}}],"answer":null}"#,
        "The focus change is awaiting confirmation.",
    ]);
    let tools = RecordingTools::default();

    let response = Agent::new("integration-agent".into())
        .run(&provider, &tools, request())
        .await
        .unwrap();

    assert_eq!(response.steps.len(), 1);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    let proposal = response.proposed_action.unwrap();
    assert_eq!(proposal.tool, "focus_window");
    assert_eq!(proposal.arguments, json!({"id":7}));
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests[1].messages[1]
            .content
            .contains("\"tool\":\"focus_window\"")
    );
    assert!(
        requests[1].messages[1]
            .content
            .contains("\"title\":\"Editor\"")
    );
}

#[tokio::test]
async fn oversized_tool_results_are_bounded_before_synthesis() {
    let provider = ScriptedProvider::with_responses([
        r#"{"steps":[{"tool":"list_windows","arguments":{}}],"answer":null}"#,
        "The result was safely bounded.",
    ]);
    let tools = RecordingTools {
        oversized_result: true,
        ..RecordingTools::default()
    };

    Agent::new("integration-agent".into())
        .run(&provider, &tools, request())
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    let synthesis = &requests[1].messages[1].content;
    assert!(synthesis.contains("[tool result truncated]"));
    assert!(synthesis.chars().count() < 17_000);
}

#[tokio::test]
async fn malformed_or_overlong_plans_fail_before_any_tool_executes() {
    for plan in [
        "```json\n{\"steps\":[]}\n```",
        r#"{"steps":[{"tool":"unknown","arguments":{}}]}"#,
        r#"{"steps":[{"tool":"list_windows","arguments":{}},{"tool":"list_windows","arguments":{}},{"tool":"list_windows","arguments":{}},{"tool":"list_windows","arguments":{}},{"tool":"list_windows","arguments":{}}]}"#,
    ] {
        let provider = ScriptedProvider::with_responses([plan]);
        let tools = RecordingTools::default();
        let error = Agent::new("integration-agent".into())
            .run(&provider, &tools, request())
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(tools.calls.lock().unwrap().is_empty());
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn tool_failure_stops_the_loop_without_requesting_synthesis() {
    let provider = ScriptedProvider::with_responses([
        r#"{"steps":[{"tool":"inspect_workspace","arguments":{"id":2}}],"answer":null}"#,
    ]);
    let tools = RecordingTools {
        fail_tool: Some("inspect_workspace"),
        ..RecordingTools::default()
    };

    let error = Agent::new("integration-agent".into())
        .run(&provider, &tools, request())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("agent tool inspect_workspace failed")
    );
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}
