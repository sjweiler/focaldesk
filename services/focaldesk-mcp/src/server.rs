use crate::backend::Backend;
use crate::policy::{Confirmation, Mutability, ToolDefinition, tool_catalog};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub struct McpServer<B> {
    backend: B,
    catalog: Vec<ToolDefinition>,
    capabilities: HashSet<String>,
    lifecycle: Lifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Uninitialized,
    Initializing,
    Ready,
}

impl<B: Backend> McpServer<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            catalog: tool_catalog(),
            capabilities: capabilities_from_env(),
            lifecycle: Lifecycle::Uninitialized,
        }
    }

    #[cfg(test)]
    fn with_capabilities(backend: B, capabilities: &[&str]) -> Self {
        Self {
            backend,
            catalog: tool_catalog(),
            capabilities: capabilities.iter().map(|value| value.to_string()).collect(),
            lifecycle: Lifecycle::Uninitialized,
        }
    }

    pub fn handle(&mut self, request: Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(error_response(
                Value::Null,
                -32600,
                "invalid JSON-RPC request",
            ));
        };
        let response_id = valid_request_id(object.get("id"));
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                response_id.unwrap_or(Value::Null),
                -32600,
                "jsonrpc must be `2.0`",
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(error_response(
                response_id.unwrap_or(Value::Null),
                -32600,
                "method must be a string",
            ));
        };
        if object.contains_key("id") && response_id.is_none() {
            return Some(error_response(
                Value::Null,
                -32600,
                "request id must be a string or integer",
            ));
        }
        let Some(id) = response_id else {
            self.handle_notification(method);
            return None;
        };

        if method != "initialize" && method != "ping" && self.lifecycle != Lifecycle::Ready {
            return Some(error_response(id, -32002, "server is not initialized"));
        }
        let result = match method {
            "initialize" => self.initialize(&request),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list_tools()),
            "tools/call" => self.call_tool(request.get("params").unwrap_or(&Value::Null)),
            _ => return Some(error_response(id, -32601, "method not found")),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    fn initialize(&mut self, request: &Value) -> Result<Value, (i64, String)> {
        if self.lifecycle != Lifecycle::Uninitialized {
            return Err((-32600, "server is already initialized".to_string()));
        }
        let params = request
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| (-32602, "initialize params are required".to_string()))?;
        for (field, expected) in [
            ("protocolVersion", "a string"),
            ("capabilities", "an object"),
            ("clientInfo", "an object"),
        ] {
            let valid = match field {
                "protocolVersion" => params.get(field).is_some_and(Value::is_string),
                _ => params.get(field).is_some_and(Value::is_object),
            };
            if !valid {
                return Err((-32602, format!("{field} must be {expected}")));
            }
        }
        let client_info = params["clientInfo"].as_object().expect("validated above");
        if !client_info.get("name").is_some_and(Value::is_string)
            || !client_info.get("version").is_some_and(Value::is_string)
        {
            return Err((
                -32602,
                "clientInfo.name and clientInfo.version must be strings".to_string(),
            ));
        }
        self.lifecycle = Lifecycle::Initializing;
        Ok(json!({
            "protocolVersion": negotiated_protocol_version(request),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {
                "name": "focaldesk-mcp",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "FocalDesk typed IPC remains authoritative. Mutating tools require an explicit session capability; sensitive actions are intentionally absent."
        }))
    }

    fn handle_notification(&mut self, method: &str) {
        if method == "notifications/initialized" && self.lifecycle == Lifecycle::Initializing {
            self.lifecycle = Lifecycle::Ready;
        }
    }

    fn list_tools(&self) -> Value {
        let tools: Vec<_> = self
            .catalog
            .iter()
            .map(|tool| {
                let read_only = tool.policy.mutability == Mutability::ReadOnly;
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": {
                        "readOnlyHint": read_only,
                        "destructiveHint": false,
                        "idempotentHint": read_only,
                        "openWorldHint": false
                    },
                    "_meta": {
                        "focaldesk/toolPolicy": tool.policy
                    }
                })
            })
            .collect();
        json!({"tools": tools})
    }

    fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| (-32602, "tool name is required".to_string()))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err((-32602, "tool arguments must be an object".to_string()));
        }
        let tool = self
            .catalog
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| (-32602, format!("unknown tool: {name}")))?;
        let started = Instant::now();
        let parameter_names = arguments
            .as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();

        if let Err(message) = validate_arguments(tool, &arguments) {
            audit(
                tool,
                "denied",
                false,
                started,
                &parameter_names,
                Some("invalid_arguments"),
            );
            return Ok(tool_error(message));
        }
        if tool.policy.mutability == Mutability::Mutating && !self.capabilities.contains(tool.name)
        {
            audit(
                tool,
                "denied",
                false,
                started,
                &parameter_names,
                Some("missing_capability"),
            );
            return Ok(tool_error(format!(
                "session capability `{}` is not granted; add it to FOCALDESK_MCP_CAPABILITIES when starting the server",
                tool.name
            )));
        }
        if tool.policy.confirmation == Confirmation::Required
            && arguments.get("confirmed").and_then(Value::as_bool) != Some(true)
        {
            audit(
                tool,
                "denied",
                false,
                started,
                &parameter_names,
                Some("confirmation_required"),
            );
            return Ok(tool_error(
                "explicit confirmation is required for this action".to_string(),
            ));
        }

        let result = self.backend.call(name, &arguments);
        audit(
            tool,
            "allowed",
            result.is_ok(),
            started,
            &parameter_names,
            result.as_ref().err().map(|_| "backend_error"),
        );
        match result {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value)
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string());
                let structured_content = match value {
                    Value::Object(_) => value,
                    other => json!({"value": other}),
                };
                Ok(json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": structured_content,
                    "isError": false
                }))
            }
            Err(message) => Ok(tool_error(message)),
        }
    }
}

fn valid_request_id(id: Option<&Value>) -> Option<Value> {
    match id {
        Some(Value::String(value)) => Some(Value::String(value.clone())),
        Some(Value::Number(value)) if value.is_i64() || value.is_u64() => {
            Some(Value::Number(value.clone()))
        }
        _ => None,
    }
}

fn negotiated_protocol_version(request: &Value) -> &str {
    request["params"]["protocolVersion"]
        .as_str()
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(MCP_PROTOCOL_VERSION)
}

fn validate_arguments(tool: &ToolDefinition, arguments: &Value) -> Result<(), String> {
    let object = arguments
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;
    let properties = tool.input_schema["properties"]
        .as_object()
        .ok_or_else(|| "tool schema is invalid".to_string())?;
    if tool.input_schema["additionalProperties"] == Value::Bool(false)
        && let Some(unknown) = object.keys().find(|key| !properties.contains_key(*key))
    {
        return Err(format!("unknown argument: {unknown}"));
    }
    if let Some(required) = tool.input_schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                return Err(format!("missing required argument: {key}"));
            }
        }
    }
    for (key, value) in object {
        let Some(schema) = properties.get(key) else {
            continue;
        };
        match schema["type"].as_str() {
            Some("boolean") if !value.is_boolean() => {
                return Err(format!("{key} must be a boolean"));
            }
            Some("string") => {
                let string = value
                    .as_str()
                    .ok_or_else(|| format!("{key} must be a string"))?;
                let length = string.chars().count() as u64;
                if schema["minLength"]
                    .as_u64()
                    .is_some_and(|minimum| length < minimum)
                {
                    return Err(format!("{key} is too short"));
                }
                if schema["maxLength"]
                    .as_u64()
                    .is_some_and(|maximum| length > maximum)
                {
                    return Err(format!("{key} is too long"));
                }
            }
            Some("integer") => {
                let number = value
                    .as_u64()
                    .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
                if schema["minimum"]
                    .as_u64()
                    .is_some_and(|minimum| number < minimum)
                {
                    return Err(format!("{key} is below the minimum"));
                }
                if schema["maximum"]
                    .as_u64()
                    .is_some_and(|maximum| number > maximum)
                {
                    return Err(format!("{key} exceeds the maximum"));
                }
            }
            _ => {}
        }
        if let Some(allowed) = schema["enum"].as_array()
            && !allowed.contains(value)
        {
            return Err(format!("{key} is not an allowed value"));
        }
    }
    Ok(())
}

fn capabilities_from_env() -> HashSet<String> {
    std::env::var("FOCALDESK_MCP_CAPABILITIES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn audit(
    tool: &ToolDefinition,
    decision: &str,
    success: bool,
    started: Instant,
    parameter_names: &[String],
    reason: Option<&str>,
) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    eprintln!(
        "{}",
        json!({
            "event": "focaldesk_mcp_tool_call",
            "timestamp_unix_ms": timestamp_ms,
            "tool": tool.name,
            "policy": tool.policy,
            "decision": decision,
            "success": success,
            "duration_ms": started.elapsed().as_millis(),
            "parameter_names": parameter_names,
            "reason": reason
        })
    );
}

fn tool_error(message: String) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

pub struct StdioTransport;

impl StdioTransport {
    pub fn run<B: Backend>(mut server: McpServer<B>) -> Result<(), String> {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            let count = input
                .read_until(b'\n', &mut line)
                .map_err(|err| err.to_string())?;
            if count == 0 {
                return Ok(());
            }
            if line.len() > MAX_MESSAGE_BYTES {
                let response = error_response(Value::Null, -32600, "MCP message exceeds 1 MiB");
                writeln!(output, "{response}").map_err(|err| err.to_string())?;
                output.flush().map_err(|err| err.to_string())?;
                continue;
            }
            let request = match serde_json::from_slice::<Value>(&line) {
                Ok(request) => request,
                Err(err) => {
                    let response =
                        error_response(Value::Null, -32700, &format!("parse error: {err}"));
                    writeln!(output, "{response}").map_err(|err| err.to_string())?;
                    output.flush().map_err(|err| err.to_string())?;
                    continue;
                }
            };
            if let Some(response) = server.handle(request) {
                writeln!(output, "{response}").map_err(|err| err.to_string())?;
                output.flush().map_err(|err| err.to_string())?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockBackend {
        calls: Mutex<Vec<String>>,
    }

    impl Backend for MockBackend {
        fn call(&self, tool: &str, _arguments: &Value) -> Result<Value, String> {
            self.calls.lock().unwrap().push(tool.to_string());
            Ok(json!({"called": tool}))
        }
    }

    fn tool_call(name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        })
    }

    fn initialize() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            }
        })
    }

    fn ready_server(capabilities: &[&str]) -> McpServer<MockBackend> {
        let mut server = McpServer::with_capabilities(MockBackend::default(), capabilities);
        assert!(server.handle(initialize()).unwrap().get("result").is_some());
        assert!(
            server
                .handle(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                }))
                .is_none()
        );
        server
    }

    #[test]
    fn read_tools_do_not_need_a_capability() {
        let mut server = ready_server(&[]);
        let response = server.handle(tool_call("list_outputs", json!({}))).unwrap();
        assert_eq!(response["result"]["isError"], false);
    }

    #[test]
    fn mutations_are_denied_without_exact_capability() {
        let mut server = ready_server(&[]);
        let response = server
            .handle(tool_call(
                "focus_window",
                json!({"window_id": 7, "confirmed": true}),
            ))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not granted")
        );
    }

    #[test]
    fn confirmation_is_enforced_after_capability_authorization() {
        let mut server = ready_server(&["focus_window"]);
        let response = server
            .handle(tool_call(
                "focus_window",
                json!({"window_id": 7, "confirmed": false}),
            ))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("confirmation")
        );
    }

    #[test]
    fn declared_schema_is_enforced_before_dispatch() {
        let mut server = ready_server(&["show_notification"]);
        let response = server
            .handle(tool_call(
                "show_notification",
                json!({"title": "hello", "body": "", "timeout_ms": 999_999}),
            ))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("maximum")
        );
    }

    #[test]
    fn tools_are_unavailable_until_initialized_notification() {
        let mut server = McpServer::with_capabilities(MockBackend::default(), &[]);
        let before_initialize = server.handle(tool_call("list_outputs", json!({}))).unwrap();
        assert_eq!(before_initialize["error"]["code"], -32002);

        server.handle(initialize()).unwrap();
        let before_notification = server.handle(tool_call("list_outputs", json!({}))).unwrap();
        assert_eq!(before_notification["error"]["code"], -32002);

        server.handle(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
        let ready = server.handle(tool_call("list_outputs", json!({}))).unwrap();
        assert_eq!(ready["result"]["isError"], false);
    }

    #[test]
    fn malformed_json_rpc_requests_are_rejected() {
        let mut server = McpServer::with_capabilities(MockBackend::default(), &[]);
        for request in [
            json!([]),
            json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1}),
        ] {
            let response = server.handle(request).unwrap();
            assert_eq!(response["error"]["code"], -32600);
        }
    }

    #[test]
    fn structured_content_is_always_an_object() {
        let mut server = ready_server(&[]);
        let response = server.handle(tool_call("list_outputs", json!({}))).unwrap();
        assert!(response["result"]["structuredContent"].is_object());
    }
}
