#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionScope {
    Once,
    Session,
    Persistent,
}

use crate::identity::AppIdentity;
use crate::request::{PermissionResource, PermissionTarget};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct PermissionState {
    pub app: AppIdentity,
    pub resource: PermissionResource,
    pub target: PermissionTarget,
    pub decision: PermissionDecision,
    pub scope: PermissionScope,
    pub updated_at: SystemTime,
}
