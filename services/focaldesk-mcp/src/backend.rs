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
            "list_outputs" => to_value(self.snapshot()?.outputs),
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
            "list_windows" => to_value(self.snapshot()?.windows),
            "list_workspaces" => to_value(self.snapshot()?.workspaces),
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
    Ok(Value::Array(
        services
            .into_iter()
            .map(|(name, socket)| {
                let state = if is_socket(&root.join(socket)) {
                    "available"
                } else {
                    "unavailable"
                };
                json!({"service": name, "state": state})
            })
            .collect(),
    ))
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
    let mut words = Vec::new();
    for word in bounded.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if [
            "password=",
            "passwd=",
            "secret=",
            "token=",
            "authorization=",
        ]
        .iter()
        .any(|marker| lower.starts_with(marker))
            || lower.starts_with("bearer:")
        {
            let key = word
                .split_once('=')
                .map(|(key, _)| key)
                .unwrap_or("credential");
            words.push(format!("{key}=[REDACTED]"));
        } else {
            words.push(word.to_string());
        }
    }
    words.join(" ")
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
}
