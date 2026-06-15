#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorIcon {
    Default,
    Pointer,
    Text,
    Crosshair,
    Grab,
    Grabbing,
    Move,
    Wait,
    Help,
    NotAllowed,
    FileDrag,
    FileDragCopy,
    EwResize,
    NsResize,
    NwseResize,
    NeswResize,
}

#[derive(Debug, Clone)]
pub struct CursorState {
    current: CursorIcon,
}

impl CursorState {
    pub fn new() -> Self {
        Self {
            current: CursorIcon::Default,
        }
    }

    pub fn current(&self) -> CursorIcon {
        self.current
    }

    pub fn set(&mut self, icon: CursorIcon) {
        self.current = icon;
    }
}

impl Default for CursorState {
    fn default() -> Self {
        Self::new()
    }
}

impl From<smithay::input::pointer::CursorIcon> for CursorIcon {
    fn from(icon: smithay::input::pointer::CursorIcon) -> Self {
        match icon {
            smithay::input::pointer::CursorIcon::Default => CursorIcon::Default,
            smithay::input::pointer::CursorIcon::Pointer => CursorIcon::Pointer,
            smithay::input::pointer::CursorIcon::Text => CursorIcon::Text,
            smithay::input::pointer::CursorIcon::Wait => CursorIcon::Wait,
            smithay::input::pointer::CursorIcon::Crosshair => CursorIcon::Crosshair,
            smithay::input::pointer::CursorIcon::Move => CursorIcon::Move,
            smithay::input::pointer::CursorIcon::NotAllowed => CursorIcon::NotAllowed,
            smithay::input::pointer::CursorIcon::EwResize => CursorIcon::EwResize,
            smithay::input::pointer::CursorIcon::NsResize => CursorIcon::NsResize,
            smithay::input::pointer::CursorIcon::NwseResize => CursorIcon::NwseResize,
            smithay::input::pointer::CursorIcon::NeswResize => CursorIcon::NeswResize,
            smithay::input::pointer::CursorIcon::Grab => CursorIcon::Grab,
            smithay::input::pointer::CursorIcon::Grabbing => CursorIcon::Grabbing,
            smithay::input::pointer::CursorIcon::Help => CursorIcon::Help,
            _ => CursorIcon::Default,
        }
    }
}
