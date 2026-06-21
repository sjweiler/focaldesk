use focaldesk_permissions::prompt::UserPromptResponse;
use focaldesk_permissions::{PermissionDecision, PermissionScope};
use focaldesk_types::OutputId;
use smithay::utils::{Logical, Rectangle};

use crate::dialog::{Dialog, DialogAction, DialogButton, DialogId, DialogKind, DialogState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiPermissionDialog {
    pub request_id: u64,
    pub title: String,
    pub message: String,
    pub allow_persistent: bool,
}

impl AiPermissionDialog {
    pub fn new(
        request_id: u64,
        title: impl Into<String>,
        message: impl Into<String>,
        allow_persistent: bool,
    ) -> Self {
        Self {
            request_id,
            title: title.into(),
            message: message.into(),
            allow_persistent,
        }
    }

    pub fn to_dialog(
        &self,
        dialog_id: DialogId,
        owner_output: OutputId,
        bounds: Rectangle<i32, Logical>,
    ) -> Dialog {
        let mut buttons = vec![
            DialogButton {
                label: "Deny".into(),
                action: DialogAction::Cancel,
            },
            DialogButton {
                label: "Allow".into(),
                action: DialogAction::Confirm,
            },
        ];

        if self.allow_persistent {
            buttons.push(DialogButton {
                label: "Remember".into(),
                action: DialogAction::Custom(1),
            });
        }

        Dialog {
            id: dialog_id,
            kind: DialogKind::Permission,
            title: self.title.clone(),
            message: self.message.clone(),
            buttons,
            modal: true,
            dismissible: false,
            state: DialogState::Open,
            owner_output,
            bounds,
        }
    }

    pub fn response_for_action(action: DialogAction) -> UserPromptResponse {
        match action {
            DialogAction::Confirm => UserPromptResponse {
                decision: PermissionDecision::Allow,
                scope: PermissionScope::Session,
            },
            DialogAction::Custom(1) => UserPromptResponse {
                decision: PermissionDecision::Allow,
                scope: PermissionScope::Persistent,
            },
            DialogAction::Cancel | DialogAction::Custom(_) => UserPromptResponse {
                decision: PermissionDecision::Deny,
                scope: PermissionScope::Session,
            },
        }
    }
}
