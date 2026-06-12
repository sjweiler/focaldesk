use crate::identity::AppIdentity;
use crate::request::{PermissionResource, PermissionTarget};
use crate::types::PermissionState;

pub trait PermissionStore: Send {
    fn get(
        &self,
        app: &AppIdentity,
        resource: PermissionResource,
        target: &PermissionTarget,
    ) -> Option<PermissionState>;

    fn set(&mut self, state: PermissionState) -> Result<(), crate::error::PermissionError>;

    fn list_for_app(&self, app: &AppIdentity) -> Vec<PermissionState>;

    fn revoke(
        &mut self,
        app: &AppIdentity,
        resource: PermissionResource,
        target: &PermissionTarget,
    ) -> Result<(), crate::error::PermissionError>;
}
