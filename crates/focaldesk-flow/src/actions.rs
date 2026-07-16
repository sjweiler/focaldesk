use focaldesk_types::WindowId;

#[derive(Debug)]
pub enum FlowAction {
    None,
    Quit,
    QuitToTTY,
    Reboot,
    PowerOff,
    Sleep,
    Focus(WindowId),
    Close(WindowId),
    Launch(String),
    ToggleLauncher,

    Spawn { cmd: String, args: Vec<String> },
    // later: MoveToWorkspace, SwitchWorkspace, etc.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    CloseFocused,
    FocusNext,
    FocusPrev,
    OverflowView,
    QuitCompositor,
    ToggleLauncher,
    LaunchTerminal,
    LockScreen,
    ActivateSlot(usize),
    AssignSlot(usize),
    TakeScreenshot,
    TakeScreenshotAll,
    LaunchBrowser,
    LaunchFiles,
    ToggleClipboardHistory,
    ToggleVoiceCapture,
}
