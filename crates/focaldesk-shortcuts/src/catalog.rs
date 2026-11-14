//! Shared Settings shortcut labels and representative default combinations.
pub const EDITABLE_SHORTCUTS: &[(&str, &str, &str)] = &[
    ("launch_terminal", "Open terminal", "Super+Enter"),
    (
        "launch_ai_console",
        "Open Desktop Agent",
        "Super+Ctrl+Shift+A",
    ),
    ("launch_browser", "Open browser", "Super+B"),
    ("toggle_launcher", "Open launcher", "Ctrl+Alt+D"),
    ("close_focused", "Close focused window", "Super+Q"),
    ("lock_screen", "Lock screen", "Super+L"),
    ("toggle_clipboard_history", "Clipboard history", "Super+V"),
    ("magnifier_zoom_in", "Increase magnification", "Super+equal"),
    (
        "magnifier_zoom_out",
        "Decrease magnification",
        "Super+minus",
    ),
    ("focus_previous", "Focus previous window", "Super+F7"),
    ("focus_next", "Focus next window", "Super+F8"),
    ("split_left", "Place window in left half", "Super+Left"),
    ("split_right", "Place window in right half", "Super+Right"),
    ("split_top", "Place window in top half", "Super+Up"),
    ("split_bottom", "Place window in bottom half", "Super+Down"),
    ("toggle_split_layout", "Choose a split layout", "Super+Z"),
    (
        "swap_split_left",
        "Swap with left split pane",
        "Super+Shift+Left",
    ),
    (
        "swap_split_right",
        "Swap with right split pane",
        "Super+Shift+Right",
    ),
    (
        "swap_split_top",
        "Swap with top split pane",
        "Super+Shift+Up",
    ),
    (
        "swap_split_bottom",
        "Swap with bottom split pane",
        "Super+Shift+Down",
    ),
    (
        "resize_split_left",
        "Move split divider left",
        "Super+Alt+Left",
    ),
    (
        "resize_split_right",
        "Move split divider right",
        "Super+Alt+Right",
    ),
    ("resize_split_top", "Move split divider up", "Super+Alt+Up"),
    (
        "resize_split_bottom",
        "Move split divider down",
        "Super+Alt+Down",
    ),
    (
        "resize_split_left_fine",
        "Nudge split divider left",
        "Super+Alt+Shift+Left",
    ),
    (
        "resize_split_right_fine",
        "Nudge split divider right",
        "Super+Alt+Shift+Right",
    ),
    (
        "resize_split_top_fine",
        "Nudge split divider up",
        "Super+Alt+Shift+Up",
    ),
    (
        "resize_split_bottom_fine",
        "Nudge split divider down",
        "Super+Alt+Shift+Down",
    ),
    ("focus_split_next", "Focus next split pane", "Super+Alt+Tab"),
    (
        "focus_split_previous",
        "Focus previous split pane",
        "Super+Alt+Shift+Tab",
    ),
    (
        "undo_split_action",
        "Undo last split action",
        "Super+Alt+BackSpace",
    ),
];
