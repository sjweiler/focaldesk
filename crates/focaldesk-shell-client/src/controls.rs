use crate::atlas::IconId;
use focaldesk_ipc::{DesktopAction, ShellSnapshot};

#[derive(Debug, Clone)]
pub struct ShellControl {
    pub icon: IconId,
    pub tooltip: String,
    pub action: DesktopAction,
    pub selected: bool,
    pub active: bool,
    pub enabled: bool,
}

impl ShellControl {
    fn new(icon: IconId, tooltip: impl Into<String>, action: DesktopAction) -> Self {
        Self {
            icon,
            tooltip: tooltip.into(),
            action,
            selected: false,
            active: false,
            enabled: true,
        }
    }

    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

pub fn panel_controls(shell: &ShellSnapshot) -> Vec<ShellControl> {
    let network_icon = if shell.network_carrier {
        if shell.wifi_signal_percent.is_some() {
            IconId::Wifi
        } else {
            IconId::Ethernet
        }
    } else if shell.wifi_signal_percent.is_some() {
        IconId::WifiOff
    } else {
        IconId::EthernetOff
    };
    let network_tooltip = match (shell.network_carrier, shell.wifi_signal_percent) {
        (true, Some(signal)) => format!("Network connected ({signal}%)"),
        (true, None) => "Ethernet connected".into(),
        (false, _) => "Network offline".into(),
    };

    vec![
        ShellControl::new(
            network_icon,
            network_tooltip,
            DesktopAction::OpenSettingsPanel {
                panel: "network".into(),
            },
        )
        .selected(shell.network_carrier),
        ShellControl::new(
            IconId::Bluetooth,
            "Bluetooth",
            DesktopAction::OpenSettingsPanel {
                panel: "bluetooth".into(),
            },
        ),
        ShellControl::new(
            IconId::Speaker,
            "Audio",
            DesktopAction::OpenSettingsPanel {
                panel: "sound".into(),
            },
        ),
        ShellControl::new(
            IconId::Notifications,
            if shell.notification_unread_count == 0 {
                "Notification center".into()
            } else {
                format!(
                    "Notification center: {} unread",
                    shell.notification_unread_count
                )
            },
            DesktopAction::OpenNotificationsPanel,
        )
        .selected(shell.notification_unread_count > 0)
        .active(shell.notification_unread_count > 0),
        ShellControl::new(
            IconId::SpeakerOff,
            if shell.do_not_disturb {
                "Do Not Disturb: on"
            } else {
                "Do Not Disturb: off"
            },
            DesktopAction::ToggleDoNotDisturb,
        )
        .selected(shell.do_not_disturb)
        .active(shell.do_not_disturb),
        ShellControl::new(
            IconId::VideoOff,
            "No camera activity detected",
            DesktopAction::OpenSettingsPanel {
                panel: "privacy".into(),
            },
        )
        .enabled(false),
        ShellControl::new(
            IconId::HDR,
            "HDR status unavailable",
            DesktopAction::OpenSettingsPanel {
                panel: "displays".into(),
            },
        )
        .enabled(false),
        ShellControl::new(
            IconId::Power,
            "Power menu",
            DesktopAction::OpenSettingsPanel {
                panel: "power".into(),
            },
        ),
    ]
}

pub fn dock_controls(
    workspace_count: usize,
    active_workspace: u32,
    capacity: usize,
) -> Vec<ShellControl> {
    let count = workspace_count.max(1);
    let fixed = 2 + 1 + usize::from(count > 1) + 4;
    let workspace_capacity = capacity.saturating_sub(fixed).max(1).min(4);
    let overflow = count > workspace_capacity;
    let shown = count.min(if overflow {
        workspace_capacity.saturating_sub(1).max(1)
    } else {
        workspace_capacity
    });

    let mut controls = vec![
        ShellControl::new(
            IconId::Settings,
            "Settings",
            DesktopAction::OpenSettingsPanel {
                panel: "appearance".into(),
            },
        ),
        ShellControl::new(
            IconId::Launcher,
            "Launcher",
            DesktopAction::LaunchApp {
                app: "@launcher".into(),
            },
        ),
    ];
    for workspace in 1..=shown {
        controls.push(
            ShellControl::new(
                IconId::Slot(workspace as u8),
                format!("Workspace {workspace}"),
                DesktopAction::FocusWorkspace {
                    workspace: workspace as u32,
                },
            )
            .selected(active_workspace == workspace as u32),
        );
    }
    controls.push(ShellControl::new(
        IconId::Plus,
        "Add new workspace",
        DesktopAction::CreateWorkspace,
    ));
    if count > 1 {
        controls.push(ShellControl::new(
            IconId::Minus,
            "Delete workspace",
            DesktopAction::DeleteWorkspace,
        ));
    }
    if overflow {
        controls.push(
            ShellControl::new(
                IconId::Overflow,
                "More workspaces",
                DesktopAction::OpenSettingsPanel {
                    panel: "workspaces".into(),
                },
            )
            .selected(active_workspace as usize > shown),
        );
    }
    controls.extend([
        ShellControl::new(
            IconId::Browser,
            "Browser",
            DesktopAction::LaunchApp {
                app: "@browser".into(),
            },
        ),
        ShellControl::new(
            IconId::Terminal,
            "Terminal",
            DesktopAction::LaunchApp {
                app: "@terminal".into(),
            },
        ),
        ShellControl::new(
            IconId::Files,
            "Files",
            DesktopAction::LaunchApp {
                app: "@files".into(),
            },
        ),
        ShellControl::new(
            IconId::Email,
            "Email",
            DesktopAction::LaunchApp {
                app: "evolution".into(),
            },
        ),
    ]);
    controls.truncate(capacity);
    controls
}

pub fn launcher_control() -> ShellControl {
    ShellControl::new(
        IconId::AiConsole,
        "Launch FocalDesk AI Console",
        DesktopAction::LaunchApp {
            app: "@ai-console".into(),
        },
    )
}

pub fn clock_control() -> ShellControl {
    ShellControl::new(
        IconId::ClockColon,
        "Calendar and clock",
        DesktopAction::OpenCalendarPanel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_restores_constructor_actions_and_runtime_states() {
        let shell = ShellSnapshot {
            network_carrier: true,
            wifi_signal_percent: Some(73),
            notification_unread_count: 3,
            do_not_disturb: true,
            ..ShellSnapshot::default()
        };
        let controls = panel_controls(&shell);
        assert_eq!(controls.len(), 8);
        assert!(controls[0].selected);
        assert!(controls[3].active);
        assert!(controls[4].active);
        assert!(!controls[5].enabled);
        assert!(!controls[6].enabled);
        assert!(matches!(
            controls[3].action,
            DesktopAction::OpenNotificationsPanel
        ));
        assert!(matches!(
            controls[4].action,
            DesktopAction::ToggleDoNotDisturb
        ));
        assert!(matches!(
            clock_control().action,
            DesktopAction::OpenCalendarPanel
        ));
        assert!(matches!(
            launcher_control().action,
            DesktopAction::LaunchApp { ref app } if app == "@ai-console"
        ));
    }

    #[test]
    fn dock_restores_workspace_and_launcher_actions() {
        let controls = dock_controls(3, 2, 12);
        assert!(matches!(
            controls[1].action,
            DesktopAction::LaunchApp { ref app } if app == "@launcher"
        ));
        assert!(controls.iter().any(|item| {
            matches!(item.action, DesktopAction::FocusWorkspace { workspace: 2 }) && item.selected
        }));
        assert!(controls
            .iter()
            .any(|item| matches!(item.action, DesktopAction::CreateWorkspace)));
        assert!(controls
            .iter()
            .any(|item| matches!(item.action, DesktopAction::DeleteWorkspace)));
    }
}
