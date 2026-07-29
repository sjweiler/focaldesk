use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutability {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    None,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    SystemMetadata,
    WindowMetadata,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditMode {
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolPolicy {
    pub access: AccessLevel,
    pub mutability: Mutability,
    pub confirmation: Confirmation,
    pub data_class: DataClass,
    pub audit: AuditMode,
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub policy: ToolPolicy,
}

fn read(data_class: DataClass) -> ToolPolicy {
    ToolPolicy {
        access: AccessLevel::Session,
        mutability: Mutability::ReadOnly,
        confirmation: Confirmation::None,
        data_class,
        audit: AuditMode::Full,
    }
}

fn mutation(confirmation: Confirmation, data_class: DataClass) -> ToolPolicy {
    ToolPolicy {
        access: AccessLevel::Session,
        mutability: Mutability::Mutating,
        confirmation,
        data_class,
        audit: AuditMode::Full,
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

pub fn tool_catalog() -> Vec<ToolDefinition> {
    let empty = || object(json!({}), &[]);
    let confirmed = json!({
        "confirmed": {
            "type": "boolean",
            "description": "True only after the user approved this specific action."
        }
    });
    vec![
        ToolDefinition {
            name: "get_session_status",
            description: "Get bounded state for the current FocalDesk session.",
            input_schema: empty(),
            policy: read(DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "list_outputs",
            description: "List connected display outputs and their current logical layout.",
            input_schema: empty(),
            policy: read(DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "get_output_details",
            description: "Get one output by numeric id or connector name.",
            input_schema: object(
                json!({
                    "output_id": {"type": "integer", "minimum": 0},
                    "connector": {"type": "string", "minLength": 1, "maxLength": 128}
                }),
                &[],
            ),
            policy: read(DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "list_windows",
            description: "List compositor-managed windows without window contents.",
            input_schema: empty(),
            policy: read(DataClass::WindowMetadata),
        },
        ToolDefinition {
            name: "list_workspaces",
            description: "List workspaces and bounded occupancy metadata.",
            input_schema: empty(),
            policy: read(DataClass::WindowMetadata),
        },
        ToolDefinition {
            name: "get_service_health",
            description: "Report availability of FocalDesk session service endpoints.",
            input_schema: empty(),
            policy: read(DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "search_recent_logs",
            description: "Search a bounded tail of recent FocalDesk logs with secret-like values redacted.",
            input_schema: object(
                json!({
                    "query": {"type": "string", "maxLength": 256},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
                }),
                &[],
            ),
            policy: read(DataClass::Diagnostics),
        },
        ToolDefinition {
            name: "get_rendering_status",
            description: "Get the compositor rendering backend and readiness state.",
            input_schema: empty(),
            policy: read(DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "show_notification",
            description: "Queue a desktop notification in the current session.",
            input_schema: object(
                json!({
                    "title": {"type": "string", "minLength": 1, "maxLength": 160},
                    "body": {"type": "string", "maxLength": 2000},
                    "timeout_ms": {"type": "integer", "minimum": 500, "maximum": 60000}
                }),
                &["title", "body"],
            ),
            policy: mutation(Confirmation::None, DataClass::SystemMetadata),
        },
        ToolDefinition {
            name: "focus_window",
            description: "Focus a compositor-managed window after explicit user confirmation.",
            input_schema: object(
                merge(
                    confirmed.clone(),
                    json!({
                        "window_id": {"type": "integer", "minimum": 1}
                    }),
                ),
                &["window_id", "confirmed"],
            ),
            policy: mutation(Confirmation::Required, DataClass::WindowMetadata),
        },
        ToolDefinition {
            name: "move_window_to_workspace",
            description: "Move a window to an existing workspace after explicit user confirmation.",
            input_schema: object(
                merge(
                    confirmed.clone(),
                    json!({
                        "window_id": {"type": "integer", "minimum": 1},
                        "workspace_id": {"type": "integer", "minimum": 1}
                    }),
                ),
                &["window_id", "workspace_id", "confirmed"],
            ),
            policy: mutation(Confirmation::Required, DataClass::WindowMetadata),
        },
        ToolDefinition {
            name: "open_settings_panel",
            description: "Open a named FocalDesk Settings panel after explicit user confirmation.",
            input_schema: object(
                merge(
                    confirmed,
                    json!({
                        "panel": {
                            "type": "string",
                            "enum": ["appearance", "network", "bluetooth", "printers", "displays",
                                     "sound", "applications", "chrome", "workspaces", "keyboard",
                                     "privacy", "power", "debug", "about"]
                        }
                    }),
                ),
                &["panel", "confirmed"],
            ),
            policy: mutation(Confirmation::Required, DataClass::SystemMetadata),
        },
    ]
}

fn merge(left: Value, right: Value) -> Value {
    let mut left = left.as_object().cloned().unwrap_or_default();
    left.extend(right.as_object().cloned().unwrap_or_default());
    Value::Object(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_has_unique_names_and_no_secret_retrieval_surface() {
        let catalog = tool_catalog();
        let names: HashSet<_> = catalog.iter().map(|tool| tool.name).collect();
        assert_eq!(names.len(), catalog.len());
        for name in names {
            assert!(!name.contains("secret"));
            assert!(!name.contains("credential"));
            assert!(!name.contains("password"));
        }
    }

    #[test]
    fn every_tool_is_fully_audited() {
        assert!(
            tool_catalog()
                .iter()
                .all(|tool| tool.policy.audit == AuditMode::Full)
        );
    }
}
