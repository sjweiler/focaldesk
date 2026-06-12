#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeAction {
    None,

    // Core UI
    OpenLauncher,
    ToggleSettings,
    ShowOverflow,

    // Slots
    SwitchToSlot(u8),
    AssignToSlot,

    // Apps
    LaunchBrowser,
    LaunchTerminal,
    LaunchFiles,

    // System
    TakeScreenshot,

    // Topbar / status
    OpenStatusMenu,
    ToggleWifi,
    ToggleBluetooth,
}

#[derive(Debug, Clone)]
pub struct ChromeItem {
    pub id: &'static str,
    pub icon: IconId,
    pub tooltip: &'static str,
    pub action: ChromeAction,
    pub visible: bool,
    pub enabled: bool,
}

