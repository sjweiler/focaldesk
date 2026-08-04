use focaldesk_ipc::{
    DesktopAction, DesktopSnapshot, IpcRequest, IpcResponse, NotificationIpcRequest,
    NotificationIpcResponse, send_desktop_request, send_notification_request,
};
use focaldesk_logging::log_file_path_candidates;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

pub trait Backend {
    fn call(&self, tool: &str, arguments: &Value) -> Result<Value, String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IpcBackend;

impl IpcBackend {
    fn snapshot(&self) -> Result<DesktopSnapshot, String> {
        match send_desktop_request(&IpcRequest::GetDesktopSnapshot)? {
            IpcResponse::DesktopSnapshot { snapshot } => Ok(snapshot),
            IpcResponse::Error { message } => Err(message),
            other => Err(format!("unexpected desktop IPC response: {other:?}")),
        }
    }

    fn action(&self, action: DesktopAction) -> Result<Value, String> {
        match send_desktop_request(&IpcRequest::ExecuteDesktopAction { action })? {
            IpcResponse::Ok => Ok(json!({"ok": true})),
            IpcResponse::Error { message } => Err(message),
            other => Err(format!("unexpected desktop IPC response: {other:?}")),
        }
    }
}

impl Backend for IpcBackend {
    fn call(&self, tool: &str, arguments: &Value) -> Result<Value, String> {
        match tool {
            "get_session_status" => to_value(self.snapshot()?.session),
            "list_outputs" => Ok(json!({"outputs": self.snapshot()?.outputs})),
            "get_output_details" => {
                let snapshot = self.snapshot()?;
                let output_id = arguments.get("output_id").and_then(Value::as_u64);
                let connector = arguments.get("connector").and_then(Value::as_str);
                if output_id.is_none() && connector.is_none() {
                    return Err("output_id or connector is required".to_string());
                }
                let output = snapshot.outputs.into_iter().find(|output| {
                    output_id.is_some_and(|id| output.id == id)
                        || connector.is_some_and(|name| output.connector == name)
                });
                to_value(output.ok_or_else(|| "output not found".to_string())?)
            }
            "list_windows" => Ok(json!({"windows": self.snapshot()?.windows})),
            "list_workspaces" => Ok(json!({"workspaces": self.snapshot()?.workspaces})),
            "get_rendering_status" => to_value(self.snapshot()?.rendering),
            "get_service_health" => service_health(),
            "search_recent_logs" => search_recent_logs(arguments),
            "show_notification" => {
                let title = required_string(arguments, "title", 160)?;
                let body = required_string_allow_empty(arguments, "body", 2_000)?;
                let timeout_ms = arguments.get("timeout_ms").and_then(Value::as_u64);
                match send_notification_request(&NotificationIpcRequest::Notify {
                    title,
                    body,
                    timeout_ms,
                })? {
                    NotificationIpcResponse::NotificationQueued { id } => {
                        Ok(json!({"ok": true, "notification_id": id}))
                    }
                    NotificationIpcResponse::Ok => Ok(json!({"ok": true})),
                    NotificationIpcResponse::Error { message } => Err(message),
                    other => Err(format!("unexpected notification IPC response: {other:?}")),
                }
            }
            "focus_window" => self.action(DesktopAction::FocusWindow {
                window_id: required_u32(arguments, "window_id")?,
            }),
            "move_window_to_workspace" => self.action(DesktopAction::MoveWindowToWorkspace {
                window_id: required_u32(arguments, "window_id")?,
                workspace: required_u32(arguments, "workspace_id")?,
            }),
            "open_settings_panel" => self.action(DesktopAction::OpenSettingsPanel {
                panel: required_string(arguments, "panel", 32)?,
            }),
            _ => Err(format!("unknown tool: {tool}")),
        }
    }
}

fn to_value(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

fn required_string(arguments: &Value, key: &str, max: usize) -> Result<String, String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    if value.is_empty() || value.len() > max {
        return Err(format!("{key} must contain 1 to {max} bytes"));
    }
    Ok(value.to_string())
}

fn required_string_allow_empty(arguments: &Value, key: &str, max: usize) -> Result<String, String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    if value.len() > max {
        return Err(format!("{key} exceeds {max} bytes"));
    }
    Ok(value.to_string())
}

fn required_u32(arguments: &Value, key: &str) -> Result<u32, String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be a positive integer"))?;
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{key} is out of range"))
}

fn service_health() -> Result<Value, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())?;
    let root = PathBuf::from(runtime).join("focaldesk");
    let services = [
        ("desktop", "desktop.sock"),
        ("settings", "settings.sock"),
        ("notifications", "notifications.sock"),
        ("power", "power.sock"),
        ("controls", "controls.sock"),
        ("dialogs", "dialog.sock"),
        ("ai", "focaldesk-ai.sock"),
        ("launcher", "focal-launchd.sock"),
    ];
    Ok(json!({
        "services": services
            .into_iter()
            .map(|(name, socket)| {
                let state = if is_socket(&root.join(socket)) {
                    "available"
                } else {
                    "unavailable"
                };
                json!({"service": name, "state": state})
            })
            .collect::<Vec<_>>()
    }))
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    path.symlink_metadata()
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn search_recent_logs(arguments: &Value) -> Result<Value, String> {
    let query = arguments.get("query").and_then(Value::as_str).unwrap_or("");
    if query.len() > 256 {
        return Err("query exceeds 256 bytes".to_string());
    }
    let query_lower = query.to_lowercase();
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let path = log_file_path_candidates()
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "no FocalDesk log file is available".to_string())?;
    let file = File::open(&path).map_err(|err| format!("could not open recent logs: {err}"))?;
    let mut matches = VecDeque::with_capacity(limit);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !query_lower.is_empty() && !line.to_lowercase().contains(&query_lower) {
            continue;
        }
        if matches.len() == limit {
            matches.pop_front();
        }
        matches.push_back(redact_log_line(&line));
    }
    Ok(json!({"lines": matches}))
}

fn redact_log_line(line: &str) -> String {
    let bounded: String = line.chars().take(4_096).collect();
    if let Ok(mut json) = serde_json::from_str::<Value>(&bounded) {
        redact_json(&mut json);
        return serde_json::to_string(&json).unwrap_or_else(|_| "[REDACTED]".to_string());
    }

    static BEARER: OnceLock<Regex> = OnceLock::new();
    static QUOTED_ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\bbearer[\s:]+[^\s,;}\]]+").expect("valid bearer redaction regex")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(password|passwd|secret|token|authorization|api[_-]?key|access[_-]?key|private[_-]?key|credential)\b[\"']?\s*[:=]\s*[\"']?[^\s,;}\]]+"#,
        )
        .expect("valid credential redaction regex")
    });
    let quoted_assignment = QUOTED_ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(password|passwd|secret|token|authorization|api[_-]?key|access[_-]?key|private[_-]?key|credential)\b[\"']?\s*[:=]\s*(\"[^\"]*\"|'[^']*')"#,
        )
        .expect("valid quoted credential redaction regex")
    });
    let redacted = bearer.replace_all(&bounded, "Bearer [REDACTED]");
    let redacted = quoted_assignment.replace_all(&redacted, "$1=[REDACTED]");
    assignment
        .replace_all(&redacted, "$1=[REDACTED]")
        .into_owned()
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) {
                    *value = Value::String("[REDACTED]".to_string());
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_json),
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "api_key",
        "access_key",
        "private_key",
        "credential",
    ]
    .iter()
    .any(|sensitive| {
        let sensitive: String = sensitive
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect();
        normalized == sensitive || normalized.ends_with(&sensitive)
    })
}

#[cfg(test)]
mod tests {
    use super::redact_log_line;

    #[test]
    fn log_search_redacts_common_secret_shapes() {
        let line = "request token=abc password=hunter2 status=failed";
        let redacted = redact_log_line(line);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("hunter2"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn log_search_redacts_nested_json_credentials() {
        let line = r#"{"request":{"api_key":"abc","safe":"visible"},"accessToken":"def"}"#;
        let redacted = redact_log_line(line);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("def"));
        assert!(redacted.contains("visible"));
    }

    #[test]
    fn log_search_redacts_colon_and_bearer_credentials() {
        let line = "authorization: Bearer abc123 api-key='def 456' status=failed";
        let redacted = redact_log_line(line);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("def 456"));
        assert!(redacted.contains("status=failed"));
    }
}
