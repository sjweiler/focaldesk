//! Explicit Settings operations. Frontends never supply commands or file paths.
use focaldesk_ai::{list_ai_permission_records, revoke_ai_permission};
use focaldesk_ipc::{send_power_request, PowerIpcRequest, PowerIpcResponse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub struct PermissionRow {
    pub id: String,
    pub app: String,
    pub description: String,
}
#[derive(Serialize)]
pub struct PermissionSnapshot {
    pub records: Vec<PermissionRow>,
    pub errors: Vec<String>,
}
fn identity(kind: &str, record: &impl std::fmt::Debug) -> String {
    format!("{kind}:{:x}", Sha256::digest(format!("{record:?}")))
}
pub fn permissions() -> PermissionSnapshot {
    let mut result = PermissionSnapshot {
        records: vec![],
        errors: vec![],
    };
    match list_ai_permission_records() {
        Ok(records) => {
            for record in records {
                result.records.push(PermissionRow {
                    id: identity("ai", &record),
                    app: record.app_identity.clone(),
                    description: format!(
                        "AI · {:?} · {:?} · {:?} · {:?}",
                        record.resource, record.target, record.decision, record.scope
                    ),
                });
            }
        }
        Err(error) => result.errors.push(format!("AI permissions: {error}")),
    }
    match crate::location_permissions::list_location_permission_records() {
        Ok(records) => {
            for record in records {
                result.records.push(PermissionRow {
                    id: identity("location", &record),
                    app: record.app_id.clone(),
                    description: format!(
                        "Location · {} · {}",
                        record.decision_label(),
                        record.accuracy_label()
                    ),
                });
            }
        }
        Err(error) => result.errors.push(error),
    }
    result
}
pub fn revoke_permission(id: &str) -> Result<(), String> {
    // Resolve the exact reviewed decision again; changed decisions require a reload.
    if id.starts_with("ai:") {
        let record = list_ai_permission_records()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|record| identity("ai", record) == id)
            .ok_or("Permission changed. Reload before revoking.")?;
        revoke_ai_permission(&record).map_err(|e| e.to_string())
    } else if id.starts_with("location:") {
        let record = crate::location_permissions::list_location_permission_records()?
            .into_iter()
            .find(|record| identity("location", record) == id)
            .ok_or("Permission changed. Reload before revoking.")?;
        crate::location_permissions::revoke_location_permission(&record)
    } else {
        Err("Unknown permission identity.".into())
    }
}
pub fn clear_recent_activity() -> Result<(), String> {
    focaldesk_launcher_state::clear_recent_activity().map_err(|e| e.to_string())
}
fn power_service_error(message: String) -> String {
    if message.contains("Connection reset by peer")
        || message.contains("Broken pipe")
        || message.contains("IPC denied")
    {
        format!("The power service refused or closed the connection. Update and restart the FocalDesk power service, then refresh status in the installed Settings app. Details: {message}")
    } else {
        message
    }
}
pub fn power_snapshot() -> Result<focaldesk_power::PowerSnapshot, String> {
    match send_power_request(&PowerIpcRequest::GetSnapshot).map_err(power_service_error)? {
        PowerIpcResponse::PowerSnapshot { snapshot } => Ok(snapshot),
        PowerIpcResponse::Error { message } => Err(power_service_error(message)),
        _ => Err("Unexpected power service response.".into()),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
}
pub fn power_action(action: PowerAction) -> Result<String, String> {
    let request = match action {
        PowerAction::Suspend => PowerIpcRequest::Suspend,
        PowerAction::Hibernate => PowerIpcRequest::Hibernate,
        PowerAction::Reboot => PowerIpcRequest::Reboot,
        PowerAction::PowerOff => PowerIpcRequest::PowerOff,
    };
    match send_power_request(&request).map_err(power_service_error)? {
        PowerIpcResponse::Ok => Ok("Power action queued. The power service will request authorization if needed; completion is not yet confirmed.".into()),
        PowerIpcResponse::Error { message } => Err(power_service_error(message)),
        _ => Err("Unexpected power service response.".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn power_connection_failures_explain_the_service_update() {
        assert!(
            power_service_error("Connection reset by peer (os error 104)".into())
                .contains("Update and restart")
        );
        assert_eq!(
            power_service_error("Authorization rejected".into()),
            "Authorization rejected"
        );
    }
    #[test]
    fn identities_include_decision_and_reject_unknown_operations() {
        assert_ne!(identity("ai", &"allow"), identity("ai", &"deny"));
        assert_ne!(identity("ai", &"allow"), identity("location", &"allow"));
        assert!(revoke_permission("unexpected").is_err());
        assert!(serde_json::from_str::<PowerAction>("\"shell\"").is_err());
    }
}
