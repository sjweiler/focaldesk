use crate::WindowId;

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

