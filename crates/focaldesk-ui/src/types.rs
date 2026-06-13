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
    AppLauncher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingKey {
    Wifi,
    Bluetooth,
    DoNotDisturb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemCommand {
    Shutdown,
    Restart,
    Logout,
    Lock,
}

#[derive(Debug, Clone)]
pub enum UiAction {
    LaunchApp(&'static str),
    ToggleSetting(SettingKey),
    SetSetting(SettingKey, bool),
    OpenPanel(PanelKind),
    CreateWorkspace(String),
    DeleteWorkspace,
    SetVolume(f32),
    SystemCommand(SystemCommand),
    Custom(ElementId),
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
    WorkspaceSlot,
    Clock,
    OutputLabel,
}
