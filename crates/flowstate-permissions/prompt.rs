use crate::request::PermissionRequest;
use crate::types::{PermissionDecision, PermissionScope};

#[derive(Debug, Clone)]
pub struct UserPromptResponse {
    pub decision: PermissionDecision,
    pub scope: PermissionScope,
}

pub trait PermissionPrompter: Send {
    fn prompt(&mut self, request: &PermissionRequest) -> UserPromptResponse;
}
