use crate::request::PermissionRequest;
use crate::types::PermissionDecision;

pub trait PolicyEngine: Send + Sync {
    fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision;
}

pub struct DefaultPolicy;

impl PolicyEngine for DefaultPolicy {
    fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        match request.resource {
            // sensitive: always ask unless explicitly stored
            crate::request::PermissionResource::Screenshot
            | crate::request::PermissionResource::Screencast
            | crate::request::PermissionResource::RemoteInput
            | crate::request::PermissionResource::ClipboardRead => PermissionDecision::Ask,

            // maybe less sensitive depending on your stance
            crate::request::PermissionResource::Notifications => PermissionDecision::Allow,

            _ => PermissionDecision::Ask,
        }
    }
}
