pub type ElementId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Network,
    Bluetooth,
    Audio,
    Display,
    Sharing,
    Recording,
    Power,
    Calendar,
    Settings,
    Workspaces,
    ClipboardHistory,
    NotificationHistory,
    Updates,
    SplitLayout,
    SplitAssist,
    SplitGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitLayoutPreset {
    LeftHalf,
    RightHalf,
    LeftTwoThirds,
    RightThird,
    LeftThird,
    RightTwoThirds,
    TopHalf,
    BottomHalf,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingKey {
    Wifi,
    Bluetooth,
    DoNotDisturb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemCommand {
    Suspend,
    Hibernate,
    Shutdown,
    Restart,
    Logout,
    Lock,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    LaunchApp(String),
    ToggleSetting(SettingKey),
    SetSetting(SettingKey, bool),
    OpenPanel(PanelKind),
    ReloadSettings,
    FocusWorkspace(u32),
    CreateWorkspace(String),
    DeleteWorkspace,
    SetVolume(f32),
    SystemCommand(SystemCommand),
    Custom(ElementId),
    SelectClipboardEntry(u64),
    ApplySplitLayout(SplitLayoutPreset),
    SelectSplitAssistWindow(focaldesk_types::WindowId),
    CancelSplitAssist,
    SwapSplitPanes,
    ReplaceSplitWindow,
    ExitSplitGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiGroup {
    Sidebar,
    TopbarLeft,
    TopbarRight,
}

#[derive(Debug, Clone)]
pub enum ElementState {
    Normal,
    Active,
    Alert,
    Disabled,
    Value(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiElementKind {
    SidebarButton,
    TopbarIndicator,
    TopbarButton,
    TopbarFlowField,
    WorkspaceSlot,
    Clock,
    OutputLabel,
}
