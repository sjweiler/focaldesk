use crate::identity::AppIdentity;
use crate::request::{PermissionResource, PermissionTarget};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GrantToken(pub String);

#[derive(Debug, Clone)]
pub struct ActiveGrant {
    pub token: GrantToken,
    pub app: AppIdentity,
    pub resource: PermissionResource,
    pub target: PermissionTarget,
    pub created_at: SystemTime,
    pub expires_at: Option<SystemTime>,
}
