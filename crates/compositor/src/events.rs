// flowos/policy/src/events.rs

use super::state::{OutputId, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetRoute {
    Wifi,
    Ethernet,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivacyDevice {
    Mic,
    Camera,
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Build,
    Debug,
}

/// Scope for search palette etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Local(OutputId),
    Global,
}

/// “Why did this happen?” helps enforce “no focus theft”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    User,      // keyboard shortcut, click, selection
    System,    // hardware change, output hotplug, app event
    App,       // client request (should rarely cause focus changes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowEvent {
    // --- Output lifecycle ---
    OutputAdded {
        output: OutputId,
        intent: Intent,
    },
    OutputRemoved {
        output: OutputId,
        intent: Intent,
        fallback_output: OutputId, // where tasks migrate
    },
    FocusOutput {
        output: OutputId,
        intent: Intent,
    },

    // --- Task lifecycle ---
    TaskCreated {
        task: TaskId,
        output: OutputId,
        intent: Intent,
        requested_slot: Option<u8>, // 1..=9
    },
    TaskClosed {
        task: TaskId,
        intent: Intent,
    },
    TaskTitleUpdated {
        task: TaskId,
        title: String,
    },

    // --- Focus/navigation ---
    FocusPinned {
        output: OutputId,
        slot: u8, // 1..=9
        intent: Intent,
    },
    FocusOverflowTask {
        output: OutputId,
        task: TaskId,
        intent: Intent,
    },
    FocusNextPinned {
        output: OutputId,
        intent: Intent,
        reverse: bool,
    },

    MoveFocusedToPinned {
        output: OutputId,
        slot: u8,
        intent: Intent,
    },

    // Move task across outputs (e.g., drag to monitor)
    MoveTaskToOutput {
        task: TaskId,
        from: OutputId,
        to: OutputId,
        intent: Intent,
        follow_focus: bool,
    },

    // --- System indicators (top bar) ---
    NetworkRouteChanged {
        route: NetRoute,
        intent: Intent,
    },
    VpnChanged {
        active: bool,
        intent: Intent,
    },
    PowerChanged {
        has_battery: bool,
        on_ac: bool,
        charging: bool,
        intent: Intent,
    },
    PrivacyChanged {
        device: PrivacyDevice,
        active: bool,
        intent: Intent,
    },
    ModeChanged {
        mode: Option<Mode>, // None means normal/release
        intent: Intent,
    },

    // Search palette scope toggle
    SearchScopeChanged {
        scope: Scope,
        intent: Intent,
    },
}
