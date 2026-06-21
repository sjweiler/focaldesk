use crate::cursor::CursorIcon;
use crate::hotspot::CursorHotspot;

pub fn hotspot_for(icon: CursorIcon) -> CursorHotspot {
    match icon {
        CursorIcon::Default => CursorHotspot::new(4, 4),
        CursorIcon::Pointer => CursorHotspot::new(6, 2),
        CursorIcon::Text => CursorHotspot::new(16, 16),
        CursorIcon::Crosshair => CursorHotspot::new(16, 16),
        CursorIcon::Move => CursorHotspot::new(16, 16),
        CursorIcon::Wait => CursorHotspot::new(16, 16),
        CursorIcon::Help => CursorHotspot::new(8, 4),
        CursorIcon::NotAllowed => CursorHotspot::new(16, 16),
        CursorIcon::FileDrag => CursorHotspot::new(4, 4),
        CursorIcon::FileDragCopy => CursorHotspot::new(4, 4),
        CursorIcon::EwResize => CursorHotspot::new(16, 16),
        CursorIcon::NsResize => CursorHotspot::new(16, 16),
        CursorIcon::NwseResize => CursorHotspot::new(16, 16),
        CursorIcon::NeswResize => CursorHotspot::new(16, 16),
        CursorIcon::Grab => CursorHotspot::new(16, 16),
        CursorIcon::Grabbing => CursorHotspot::new(16, 16),
    }
}
