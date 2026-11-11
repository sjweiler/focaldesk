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
        let mut tools: Vec<AgentToolSpec> = tool_catalog()
            .into_iter()
            .map(|tool| AgentToolSpec {
                name: tool.name.to_string(),
                description: tool.description.to_string(),
                input_schema: planner_schema(tool.input_schema),
                mutating: tool.policy.mutability == Mutability::Mutating,
            })
            .collect();
        tools.extend(focaldesk_browserd::tool_definitions().into_iter().map(
            |(name, description, input_schema, mutating)| AgentToolSpec {
                name: name.into(),
                description: description.into(),
                input_schema,
                mutating,
            },
        ));
        tools
    }

    async fn execute(&self, tool: &str, arguments: Value) -> Result<Value> {
        if tool.starts_with("browser_") {
            if matches!(tool, "browser_click" | "browser_type") {
                anyhow::bail!("Browser interaction requires explicit confirmation");
            }
            return execute_browser(tool, arguments).await;
        }
        let tool = tool.to_string();
        tokio::task::spawn_blocking(move || {
            execute_read_only_tool(&IpcBackend, &tool, &arguments).map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|err| anyhow!("agent tool task failed: {err}"))?
    }

    async fn execute_confirmed(&self, tool: &str, arguments: Value) -> Result<Value> {
        if tool.starts_with("browser_") {
            return execute_browser(tool, arguments).await;
        }
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

async fn execute_browser(tool: &str, arguments: Value) -> Result<Value> {
    let request = focaldesk_browserd::request_for_tool(tool, arguments)?;
    tokio::task::spawn_blocking(move || {
        let mut result = focaldesk_browserd::send(&request)?;
        // Images are displayed by the browser panel; base64 is not useful text context.
        if let Some(object) = result.as_object_mut() {
            if object.remove("data_url").is_some() {
                object.insert("screenshot_captured".into(), Value::Bool(true));
            }
        }
        Ok(compact_browser_result(result))
    })
    .await?
}

// Stay below the agent's per-tool context limit without turning a structured
// page result into a truncated string and losing its page_id/selectors.
fn compact_browser_result(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        for (field, limit) in [("text", 6000), ("title", 512), ("url", 2048)] {
            if let Some(text) = object.get(field).and_then(Value::as_str) {
                if text.chars().count() > limit {
                    let text = text.chars().take(limit).collect::<String>();
                    object.insert(field.into(), Value::String(text));
                    object.insert("truncated".into(), Value::Bool(true));
                }
            }
        }
        if let Some(elements) = object.get_mut("elements").and_then(Value::as_array_mut) {
            elements.truncate(20);
        }
    }
    while serde_json::to_string(&result)
        .unwrap_or_default()
        .chars()
        .count()
        > 14_000
    {
        if let Some(elements) = result.get_mut("elements").and_then(Value::as_array_mut) {
            if elements.pop().is_some() {
                result["truncated"] = Value::Bool(true);
                continue;
            }
        }
        if let Some(text) = result.get("text").and_then(Value::as_str) {
            let text = text
                .chars()
                .take(text.chars().count() / 2)
                .collect::<String>();
            result["text"] = Value::String(text);
            result["truncated"] = Value::Bool(true);
        } else {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_browser_observations_keep_page_identity_and_valid_json() {
        let input = serde_json::json!({"page_id":"tab-123","title":"A page","url":"https://example.com","text":"\"".repeat(32000),"elements":(0..100).map(|i|serde_json::json!({"selector":format!("#{i}"),"text":"large".repeat(200)})).collect::<Vec<_>>()});
        let result = compact_browser_result(input);
        assert_eq!(result["page_id"], "tab-123");
        assert!(result.is_object());
        assert_eq!(result["truncated"], true);
        assert!(serde_json::to_string(&result).unwrap().chars().count() <= 14000);
    }
    #[tokio::test]
    async fn browser_interactions_cannot_execute_without_confirmation() {
        for tool in ["browser_click", "browser_type"] {
            let error = McpAgentTools
                .execute(
                    tool,
                    serde_json::json!({"page_id":"test","selector":"button"}),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("explicit confirmation"));
        }
    }
    #[test]
    fn browser_catalog_retains_mutation_policy() {
        let tools = McpAgentTools.tools();
        for name in [
            "browser_open",
            "browser_read",
            "browser_screenshot",
            "browser_click",
            "browser_type",
            "browser_close",
        ] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert_eq!(
                tool.mutating,
                matches!(name, "browser_click" | "browser_type")
            );
        }
    }
}
