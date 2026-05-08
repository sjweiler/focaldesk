use crate::WindowId;

pub enum FlowEvent {
    WindowMapped { id: WindowId },
    WindowUnmapped { id: WindowId },
    FocusChanged { id: Option<WindowId> },
    CloseFocused,
    ToggleLauncher,
    Quit,
    QuitToTTY,
    Reboot,
    PowerOff,
    Sleep,
    FocusNext,
    Key { combo: String },
    PointerMoved { x: f64, y: f64 }, // optional
}
