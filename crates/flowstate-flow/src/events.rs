// flowos/policy/src/events.rs

use flowstate_types::OutputId;

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
    User,   // keyboard shortcut, click, selection
    System, // hardware change, output hotplug, app event
    App,    // client request (should rarely cause focus changes)
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

    // --- Focus/navigation ---
    FocusPinned {
        output: OutputId,
        slot: u8, // 1..=9
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
