use crate::identity::AppMetadata;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionResource {
    Screenshot,
    Screencast,
    ScreenShareWindow,
    ScreenShareOutput,
    Microphone,
    Camera,
    ClipboardRead,
    ClipboardWrite,
    RemoteInput,
    Notifications,
    FileOpen,
    FileSave,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PermissionTarget {
    Global,
    Named(String),
}

#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub app: AppMetadata,
    pub resource: PermissionResource,
    pub target: PermissionTarget,
}
