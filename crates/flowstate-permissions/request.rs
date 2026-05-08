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



