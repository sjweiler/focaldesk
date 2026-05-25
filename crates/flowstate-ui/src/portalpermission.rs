use flowstate_permissions::identity::AppIdentity;
use flowstate_permissions::prompt::UserPromptResponse;
use flowstate_permissions::request::{PermissionRequest, PermissionResource, PermissionTarget};
use flowstate_permissions::{PermissionDecision, PermissionScope};
use flowstate_types::OutputId;
use smithay::utils::{Logical, Rectangle};

use crate::dialog::{Dialog, DialogAction, DialogButton, DialogId, DialogKind, DialogState};

pub type PortalRequestId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalPermissionDialog {
    pub request_id: PortalRequestId,
    pub app_name: String,
    pub app_id: Option<String>,
    pub permission: PortalPermissionKind,
    pub target: PortalTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalPermissionKind {
    ScreenCast,
    Screenshot,
    RemoteDesktop,
    FileChooser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalTarget {
    Global,
    Named(String),
}

impl PortalPermissionDialog {
    pub fn new(
        request_id: PortalRequestId,
        app_name: impl Into<String>,
        app_id: Option<String>,
        permission: PortalPermissionKind,
        target: PortalTarget,
    ) -> Self {
        Self {
            request_id,
            app_name: app_name.into(),
            app_id,
            permission,
            target,
        }
    }

    pub fn from_permission_request(
        request_id: PortalRequestId,
        request: &PermissionRequest,
    ) -> Option<Self> {
        Some(Self {
            request_id,
            app_name: app_name_for_request(request),
            app_id: app_id_for_identity(&request.app.identity),
            permission: PortalPermissionKind::from_resource(request.resource)?,
            target: PortalTarget::from(&request.target),
        })
    }

    pub fn to_dialog(
        &self,
        dialog_id: DialogId,
        owner_output: OutputId,
        bounds: Rectangle<i32, Logical>,
    ) -> Dialog {
        Dialog {
            id: dialog_id,
            kind: DialogKind::Permission,
            title: self.title(),
            message: self.message(),
            buttons: vec![
                DialogButton {
                    label: "Deny".into(),
                    action: DialogAction::Cancel,
                },
                DialogButton {
                    label: "Allow".into(),
                    action: DialogAction::Confirm,
                },
            ],
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
            DialogAction::Cancel | DialogAction::Custom(_) => UserPromptResponse {
                decision: PermissionDecision::Deny,
                scope: PermissionScope::Session,
            },
        }
    }

    pub fn title(&self) -> String {
        format!("{} wants {}", self.app_name, self.permission.action_label())
    }

    pub fn message(&self) -> String {
        let target = self.target.label();
        match &self.app_id {
            Some(app_id) => format!("App ID: {app_id}. Target: {target}."),
            None => format!("Target: {target}."),
        }
    }
}

impl PortalPermissionKind {
    pub fn from_resource(resource: PermissionResource) -> Option<Self> {
        match resource {
            PermissionResource::Screenshot => Some(Self::Screenshot),
            PermissionResource::Screencast
            | PermissionResource::ScreenShareWindow
            | PermissionResource::ScreenShareOutput => Some(Self::ScreenCast),
            PermissionResource::RemoteInput => Some(Self::RemoteDesktop),
            PermissionResource::FileOpen | PermissionResource::FileSave => Some(Self::FileChooser),
            PermissionResource::Microphone
            | PermissionResource::Camera
            | PermissionResource::ClipboardRead
            | PermissionResource::ClipboardWrite
            | PermissionResource::Notifications => None,
        }
    }

    pub fn action_label(self) -> &'static str {
        match self {
            Self::ScreenCast => "to share the screen",
            Self::Screenshot => "to take a screenshot",
            Self::RemoteDesktop => "remote desktop control",
            Self::FileChooser => "file access",
        }
    }
}

impl PortalTarget {
    pub fn label(&self) -> &str {
        match self {
            Self::Global => "all screens",
            Self::Named(name) => name.as_str(),
        }
    }
}

impl From<&PermissionTarget> for PortalTarget {
    fn from(target: &PermissionTarget) -> Self {
        match target {
            PermissionTarget::Global => Self::Global,
            PermissionTarget::Named(name) => Self::Named(name.clone()),
        }
    }
}

fn app_name_for_request(request: &PermissionRequest) -> String {
    request
        .app
        .window_title
        .clone()
        .or_else(|| app_id_for_identity(&request.app.identity))
        .unwrap_or_else(|| "An application".into())
}

fn app_id_for_identity(identity: &AppIdentity) -> Option<String> {
    match identity {
        AppIdentity::DesktopId(id)
        | AppIdentity::FlatpakId(id)
        | AppIdentity::WaylandAppId(id)
        | AppIdentity::ExecutablePath(id) => Some(id.clone()),
        AppIdentity::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowstate_permissions::identity::AppMetadata;

    #[test]
    fn builds_portal_dialog_from_permission_request() {
        let request = PermissionRequest {
            app: AppMetadata {
                identity: AppIdentity::WaylandAppId("org.example.App".into()),
                pid: Some(42),
                window_title: Some("Example".into()),
                sandboxed: false,
            },
            resource: PermissionResource::Screencast,
            target: PermissionTarget::Named("HDMI-A-1".into()),
        };

        let portal = PortalPermissionDialog::from_permission_request(7, &request)
            .expect("screencast is portal-backed");

        assert_eq!(portal.request_id, 7);
        assert_eq!(portal.app_name, "Example");
        assert_eq!(portal.app_id.as_deref(), Some("org.example.App"));
        assert_eq!(portal.permission, PortalPermissionKind::ScreenCast);
        assert_eq!(portal.target, PortalTarget::Named("HDMI-A-1".into()));
    }

    #[test]
    fn maps_dialog_actions_to_prompt_responses() {
        let allow = PortalPermissionDialog::response_for_action(DialogAction::Confirm);
        assert_eq!(allow.decision, PermissionDecision::Allow);
        assert_eq!(allow.scope, PermissionScope::Session);

        let deny = PortalPermissionDialog::response_for_action(DialogAction::Cancel);
        assert_eq!(deny.decision, PermissionDecision::Deny);
        assert_eq!(deny.scope, PermissionScope::Session);
    }
}
