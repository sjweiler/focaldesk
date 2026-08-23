use anyhow::{anyhow, Result};
use async_trait::async_trait;
use focaldesk_ai::{AgentToolExecutor, AgentToolSpec};
use focaldesk_mcp::{
    execute_confirmed_tool, execute_read_only_tool, tool_catalog, IpcBackend, Mutability,
};
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub struct McpAgentTools;

#[async_trait]
impl AgentToolExecutor for McpAgentTools {
    fn tools(&self) -> Vec<AgentToolSpec> {
        tool_catalog()
            .into_iter()
            .map(|tool| AgentToolSpec {
                name: tool.name.to_string(),
                description: tool.description.to_string(),
                input_schema: planner_schema(tool.input_schema),
                mutating: tool.policy.mutability == Mutability::Mutating,
            })
            .collect()
    }

    async fn execute(&self, tool: &str, arguments: Value) -> Result<Value> {
        let tool = tool.to_string();
        tokio::task::spawn_blocking(move || {
            execute_read_only_tool(&IpcBackend, &tool, &arguments).map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|err| anyhow!("agent tool task failed: {err}"))?
    }

    async fn execute_confirmed(&self, tool: &str, arguments: Value) -> Result<Value> {
        let tool = tool.to_string();
        tokio::task::spawn_blocking(move || {
            execute_confirmed_tool(&IpcBackend, &tool, &arguments).map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|err| anyhow!("confirmed agent tool task failed: {err}"))?
    }
}

fn planner_schema(mut schema: Value) -> Value {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove("confirmed");
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| name.as_str() != Some("confirmed"));
    }
    schema
}
