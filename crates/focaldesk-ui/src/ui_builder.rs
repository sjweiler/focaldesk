use crate::accessibility::{AccessibleInfo, AccessibleRole};
use crate::atlas::IconId;
use crate::chrome_layout::{ChromeLayout, ChromeLayoutConfig, DEFAULT_SIDEBAR_SLOT_COUNT};
use crate::element::{ChromeItem, UiElement, UiRect};
use crate::sidebar::SideBar;
use crate::topbar::TopBar;
use crate::types::{PanelKind, UiAction, UiElementKind};
use crate::uitree::UiTree;
use focaldesk_network::model::{Connectivity, NetworkIcon as NetIcon, NetworkState, map_icon};

const SIDEBAR_BASE: u32 = 1_000;
const CLOCK_ID: u32 = 100_000;
pub const TOPBAR_FLOW_FIELD_ID: u32 = 100_001;
pub const SIDEBAR_SETTINGS_ID: u32 = SIDEBAR_BASE + 1;
pub const SIDEBAR_ADD_WORKSPACE_ID: u32 = SIDEBAR_BASE + 3;
pub const SIDEBAR_DELETE_WORKSPACE_ID: u32 = SIDEBAR_BASE + 4;
pub const SIDEBAR_BROWSER_ID: u32 = SIDEBAR_BASE + 5;
pub const SIDEBAR_TERMINAL_ID: u32 = SIDEBAR_BASE + 6;
pub const SIDEBAR_FILES_ID: u32 = SIDEBAR_BASE + 7;
pub const SIDEBAR_EMAIL_ID: u32 = SIDEBAR_BASE + 8;
pub const SIDEBAR_WORKSPACE_OVERFLOW_ID: u32 = SIDEBAR_BASE + 10;
pub const TOPBAR_NETWORK_ID: u32 = 100;
pub const TOPBAR_BLUETOOTH_ID: u32 = 101;
pub const TOPBAR_AUDIO_ID: u32 = 102;
pub const TOPBAR_DISPLAY_ID: u32 = 103;
pub const TOPBAR_POWER_ID: u32 = 104;
pub const TOPBAR_CAMERA_ID: u32 = 105;
pub const TOPBAR_DND_ID: u32 = 106;
pub const TOPBAR_NOTIFICATIONS_ID: u32 = 107;

// Workspace buttons get dynamically assigned IDs instead of fixed per-slot
// consts, since the number of individually displayed workspace buttons is
// configurable (see UiBuildOptions::max_workspace_slots) rather than fixed at 3.
pub const SIDEBAR_WORKSPACE_ID_BASE: u32 = SIDEBAR_BASE + 100;
const SIDEBAR_WORKSPACE_ID_SLOTS: u32 = 32;

pub fn sidebar_workspace_id(workspace_number: u32) -> u32 {
    debug_assert!(workspace_number >= 1);
    SIDEBAR_WORKSPACE_ID_BASE + (workspace_number - 1)
}

/// Decodes a sidebar element id back into a 1-based workspace number, if it
/// falls within the dynamic workspace id range.
pub fn sidebar_workspace_number(id: u32) -> Option<u32> {
    if id >= SIDEBAR_WORKSPACE_ID_BASE
        && id < SIDEBAR_WORKSPACE_ID_BASE + SIDEBAR_WORKSPACE_ID_SLOTS
    {
        Some(id - SIDEBAR_WORKSPACE_ID_BASE + 1)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiFlowMode {
    Idle,
    Thinking,
    Acting,
    PermissionWait,
    Error,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCaptureStatus {
    #[default]
    Unavailable,
    Idle,
    Starting,
    Listening,
    Stopping,
}

#[derive(Debug, Clone)]
pub struct UiBuildOptions {
    pub hdr_supported: bool,
    pub hdr_requested: bool,
    pub hdr_kms_applied: bool,
    pub microphone_detected: bool,
    pub voice_capture_status: VoiceCaptureStatus,
    pub camera_detected: bool,
    pub camera_active: bool,
    pub do_not_disturb: bool,
    pub notification_unread: bool,
    pub notification_unread_count: usize,
    pub network_state: NetworkState,
    pub workspace_count: usize,
    /// Max number of workspace buttons shown individually before they
    /// collapse into an overflow button. Mirrors
    /// `WorkspaceSettings::max_workspace_slots`.
    pub max_workspace_slots: usize,
    pub active_workspace: u32,
    pub ai_flow_mode: AiFlowMode,
    /// Replaces the generated sidebar collection when supplied. Invisible
    /// items do not consume layout slots.
    pub sidebar_items: Option<Vec<ChromeItem>>,
    /// Replaces the generated status collection when supplied. Invisible
    /// items do not consume layout wells.
    pub status_items: Option<Vec<ChromeItem>>,
}

impl Default for UiBuildOptions {
    fn default() -> Self {
        Self {
            hdr_supported: false,
            hdr_requested: false,
            hdr_kms_applied: false,
            microphone_detected: false,
            voice_capture_status: VoiceCaptureStatus::Unavailable,
            camera_detected: false,
            camera_active: false,
            do_not_disturb: false,
            notification_unread: false,
            notification_unread_count: 0,
            network_state: NetworkState::default(),
            workspace_count: 1,
            max_workspace_slots: 4,
            active_workspace: 1,
            ai_flow_mode: AiFlowMode::Idle,
            sidebar_items: None,
            status_items: None,
        }
    }
}

impl UiBuildOptions {
    /// Layout capacity required by the currently supplied runtime collections.
    /// Built-in content retains the legacy capacities; replacement collections
    /// allocate one slot/well per visible item.
    pub fn layout_config(&self) -> ChromeLayoutConfig {
        ChromeLayoutConfig {
            status_item_count: self
                .status_items
                .as_ref()
                .map(|items| items.iter().filter(|item| item.visible).count())
                .unwrap_or_else(|| {
                    default_status_items(self)
                        .iter()
                        .filter(|item| item.visible)
                        .count()
                }),
            sidebar_item_count: self
                .sidebar_items
                .as_ref()
                .map(|items| items.iter().filter(|item| item.visible).count())
                .unwrap_or(DEFAULT_SIDEBAR_SLOT_COUNT),
        }
    }
}

fn hdr_tooltip(hdr_supported: bool, hdr_requested: bool, hdr_kms_applied: bool) -> &'static str {
    if !hdr_supported {
        "HDR not detected"
    } else if hdr_kms_applied && hdr_requested {
        "HDR active (KMS live)"
    } else if hdr_requested {
        "HDR requested (pending KMS)"
    } else {
        "HDR supported (off)"
    }
}

/// [`NetworkIcon`](focaldesk_network::model::NetworkIcon) has 13 variants
/// (signal-strength steps, VPN badge) but the icon atlas only has plain
/// Wifi/WifiOff/Ethernet/EthernetOff glyphs — no signal-strength or VPN
/// artwork exists yet. This collapses onto what's actually drawable; the
/// tooltip carries the detail the icon can't.
fn network_icon(state: &NetworkState) -> IconId {
    match map_icon(state) {
        NetIcon::Offline | NetIcon::Connecting | NetIcon::Limited => IconId::WifiOff,
        NetIcon::Ethernet | NetIcon::EthernetVpn => IconId::Ethernet,
        NetIcon::Wifi0
        | NetIcon::Wifi1
        | NetIcon::Wifi2
        | NetIcon::Wifi3
        | NetIcon::Wifi4
        | NetIcon::WifiVpn0
        | NetIcon::WifiVpn1
        | NetIcon::WifiVpn2
        | NetIcon::WifiVpn3
        | NetIcon::WifiVpn4 => IconId::Wifi,
    }
}

fn network_tooltip(state: &NetworkState) -> String {
    use focaldesk_network::model::NetTransport;

    let vpn_suffix = if state.vpn_active {
        " · VPN active"
    } else {
        ""
    };

    match state.connectivity {
        Connectivity::Unknown | Connectivity::Disconnected => "Offline".to_string(),
        Connectivity::Connecting => "Connecting…".to_string(),
        Connectivity::LinkOnly => "No IP address".to_string(),
        Connectivity::LocalOnly => "No internet access".to_string(),
        Connectivity::SiteOnly => "Limited connectivity".to_string(),
        Connectivity::Internet => match state.primary_transport {
            Some(NetTransport::Wifi) => {
                let ssid = state
                    .wifi
                    .as_ref()
                    .and_then(|wifi| wifi.ssid.as_deref())
                    .unwrap_or("Wifi");
                let signal = state
                    .wifi
                    .as_ref()
                    .and_then(|wifi| wifi.signal_percent)
                    .map(|percent| format!(" ({percent}%)"))
                    .unwrap_or_default();
                format!("{ssid}{signal}{vpn_suffix}")
            }
            Some(NetTransport::Ethernet) => format!("Ethernet{vpn_suffix}"),
            Some(NetTransport::Vpn) => "VPN active".to_string(),
            Some(NetTransport::Cellular) => format!("Cellular{vpn_suffix}"),
            Some(NetTransport::Unknown) | None => format!("Connected{vpn_suffix}"),
        },
    }
}

pub fn default_status_items(options: &UiBuildOptions) -> Vec<ChromeItem> {
    let (audio_icon, audio_tooltip, audio_selected, audio_active) =
        match (options.microphone_detected, options.voice_capture_status) {
            (_, VoiceCaptureStatus::Starting) => {
                (IconId::Microphone, "Voice input: starting", true, false)
            }
            (_, VoiceCaptureStatus::Listening) => (
                IconId::Microphone,
                "Voice input: listening — Super+Shift+V to stop",
                true,
                true,
            ),
            (_, VoiceCaptureStatus::Stopping) => {
                (IconId::Microphone, "Voice input: stopping", false, false)
            }
            (true, VoiceCaptureStatus::Idle) => (
                IconId::MicrophoneOff,
                "Voice input: not listening — Super+Shift+V to start",
                false,
                false,
            ),
            (true, VoiceCaptureStatus::Unavailable) => (
                IconId::MicrophoneOff,
                "Voice input unavailable",
                false,
                false,
            ),
            (false, VoiceCaptureStatus::Idle | VoiceCaptureStatus::Unavailable) => {
                (IconId::Speaker, "Audio", false, false)
            }
        };

    let network_selected = matches!(options.network_state.connectivity, Connectivity::Internet);
    let network_active = matches!(
        options.network_state.connectivity,
        Connectivity::Connecting
            | Connectivity::LinkOnly
            | Connectivity::LocalOnly
            | Connectivity::SiteOnly
    );

    vec![
        ChromeItem::new(
            TOPBAR_NETWORK_ID,
            network_icon(&options.network_state),
            network_tooltip(&options.network_state),
            UiAction::OpenPanel(PanelKind::Network),
        )
        .selected(network_selected)
        .active(network_active),
        ChromeItem::new(
            TOPBAR_BLUETOOTH_ID,
            IconId::Bluetooth,
            "Bluetooth",
            UiAction::OpenPanel(PanelKind::Bluetooth),
        ),
        ChromeItem::new(
            TOPBAR_AUDIO_ID,
            audio_icon,
            audio_tooltip,
            UiAction::OpenPanel(PanelKind::Audio),
        )
        .selected(audio_selected)
        .active(audio_active),
        ChromeItem::new(
            TOPBAR_NOTIFICATIONS_ID,
            IconId::Notifications,
            match options.notification_unread_count {
                count if count > 0 => format!("Notification center: {count} unread notifications"),
                _ => "Notification center".to_string(),
            },
            UiAction::OpenPanel(PanelKind::NotificationHistory),
        )
        .selected(options.notification_unread)
        .active(options.notification_unread),
        ChromeItem::new(
            TOPBAR_DND_ID,
            IconId::SpeakerOff,
            if options.do_not_disturb {
                "Do Not Disturb: on"
            } else {
                "Do Not Disturb: off"
            },
            UiAction::Custom(TOPBAR_DND_ID),
        )
        .selected(options.do_not_disturb)
        .active(options.do_not_disturb),
        ChromeItem::new(
            TOPBAR_CAMERA_ID,
            if options.camera_active {
                IconId::Video
            } else {
                IconId::VideoOff
            },
            if options.camera_active {
                "Camera in use"
            } else if options.camera_detected {
                "Camera detected — not in use"
            } else {
                "No camera detected"
            },
            UiAction::OpenPanel(PanelKind::Settings),
        )
        .selected(options.camera_detected)
        .active(options.camera_active),
        ChromeItem::new(
            TOPBAR_DISPLAY_ID,
            IconId::HDR,
            hdr_tooltip(
                options.hdr_supported,
                options.hdr_requested,
                options.hdr_kms_applied,
            ),
            UiAction::OpenPanel(PanelKind::Display),
        )
        .selected(options.hdr_kms_applied)
        .active(options.hdr_requested && !options.hdr_kms_applied),
        ChromeItem::new(
            TOPBAR_POWER_ID,
            IconId::Power,
            "Power menu",
            UiAction::OpenPanel(PanelKind::Power),
        ),
    ]
}

pub fn default_sidebar_items(options: &UiBuildOptions, total_slots: usize) -> Vec<ChromeItem> {
    let workspace_count = options.workspace_count.max(1);
    let fixed_before = 2; // settings, launcher
    let add_slot = 1;
    let remove_slot = usize::from(workspace_count > 1);
    let fixed_after = 4; // browser, terminal, files, email
    let reserved_no_overflow = fixed_before + add_slot + remove_slot + fixed_after;
    let slots_for_workspaces_no_overflow = total_slots.saturating_sub(reserved_no_overflow).max(1);

    // The atlas currently defines Slot(1)..=Slot(9).
    let max_workspace_slots = options.max_workspace_slots.clamp(1, 9);
    let workspace_cap_no_overflow = max_workspace_slots.min(slots_for_workspaces_no_overflow);
    let (displayed_workspace_count, show_overflow) = if workspace_count <= workspace_cap_no_overflow
    {
        (workspace_count, false)
    } else {
        let slots_for_workspaces_with_overflow =
            total_slots.saturating_sub(reserved_no_overflow + 1).max(1);
        let cap = max_workspace_slots.min(slots_for_workspaces_with_overflow);
        (workspace_count.min(cap), true)
    };

    let mut items = vec![
        ChromeItem::new(
            SIDEBAR_SETTINGS_ID,
            IconId::Settings,
            "Settings",
            UiAction::Custom(SIDEBAR_SETTINGS_ID),
        ),
        ChromeItem::new(
            SIDEBAR_BASE,
            IconId::Launcher,
            "Launcher",
            UiAction::LaunchApp("@launcher".into()),
        ),
    ];

    for workspace in 1..=displayed_workspace_count {
        let id = sidebar_workspace_id(workspace as u32);
        items.push(
            ChromeItem::new(
                id,
                IconId::Slot(workspace as u8),
                format!("Workspace {workspace}"),
                UiAction::Custom(id),
            )
            .selected(options.active_workspace == workspace as u32),
        );
    }

    items.push(ChromeItem::new(
        SIDEBAR_ADD_WORKSPACE_ID,
        IconId::Plus,
        "Add new workspace",
        UiAction::Custom(SIDEBAR_ADD_WORKSPACE_ID),
    ));
    if workspace_count > 1 {
        items.push(ChromeItem::new(
            SIDEBAR_DELETE_WORKSPACE_ID,
            IconId::Minus,
            "Delete workspace",
            UiAction::Custom(SIDEBAR_DELETE_WORKSPACE_ID),
        ));
    }
    if show_overflow {
        items.push(
            ChromeItem::new(
                SIDEBAR_WORKSPACE_OVERFLOW_ID,
                IconId::Overflow,
                "More workspaces",
                UiAction::OpenPanel(PanelKind::Workspaces),
            )
            .selected(options.active_workspace > displayed_workspace_count as u32),
        );
    }
    items.extend([
        ChromeItem::new(
            SIDEBAR_BROWSER_ID,
            IconId::Browser,
            "Browser",
            UiAction::Custom(SIDEBAR_BROWSER_ID),
        ),
        ChromeItem::new(
            SIDEBAR_TERMINAL_ID,
            IconId::Terminal,
            "Terminal",
            UiAction::Custom(SIDEBAR_TERMINAL_ID),
        ),
        ChromeItem::new(
            SIDEBAR_FILES_ID,
            IconId::Files,
            "Files",
            UiAction::Custom(SIDEBAR_FILES_ID),
        ),
        ChromeItem::new(
            SIDEBAR_EMAIL_ID,
            IconId::Email,
            "Email",
            UiAction::Custom(SIDEBAR_EMAIL_ID),
        ),
    ]);
    items
}

pub fn build_ui_for_output(ui: &mut UiTree, layout: &ChromeLayout) {
    build_ui_for_output_with_options(ui, layout, UiBuildOptions::default());
}

pub fn build_ui_for_output_with_options(
    ui: &mut UiTree,
    layout: &ChromeLayout,
    options: UiBuildOptions,
) {
    ui.elements.clear();

    let (flow_selected, flow_active, flow_enabled) = match options.ai_flow_mode {
        AiFlowMode::Idle => (false, false, true),
        AiFlowMode::Thinking => (true, false, true),
        AiFlowMode::Acting => (false, true, true),
        AiFlowMode::PermissionWait => (true, true, true),
        AiFlowMode::Error => (false, false, false),
    };

    let sidebar_entries = options
        .sidebar_items
        .clone()
        .unwrap_or_else(|| default_sidebar_items(&options, layout.sidebar.slots.len()));

    ui.elements
        .extend(SideBar::layout_items(layout, sidebar_entries));

    let mut flow_field = UiElement::topbar_indicator(
        TOPBAR_FLOW_FIELD_ID,
        IconId::AiConsole,
        "Launch FocalDesk AI Console",
    );
    flow_field.kind = UiElementKind::TopbarFlowField;
    flow_field.action = Some(UiAction::LaunchApp("focaldesk-ai-console".into()));
    flow_field.accessible = Some(
        AccessibleInfo::new(AccessibleRole::Button, "FocalDesk AI Console")
            .description("Open the AI Console"),
    );
    flow_field.bounds = UiRect {
        x: layout.topbar.flow_field.loc.x,
        y: layout.topbar.flow_field.loc.y,
        w: layout.topbar.flow_field.size.w,
        h: layout.topbar.flow_field.size.h,
    };
    flow_field.hover_scale = 1.0;
    flow_field.press_scale = 1.0;
    flow_field.selected = flow_selected;
    flow_field.active = flow_active;
    flow_field.enabled = flow_enabled;
    ui.elements.push(flow_field);

    let status_items = options
        .status_items
        .clone()
        .unwrap_or_else(|| default_status_items(&options));
    ui.elements
        .extend(TopBar::layout_status_items(layout, status_items));

    ui.elements.push(UiElement {
        id: CLOCK_ID,
        kind: UiElementKind::Clock,
        bounds: UiRect {
            x: layout.topbar.clock_well.loc.x,
            y: layout.topbar.clock_well.loc.y,
            w: layout.topbar.clock_well.size.w,
            h: layout.topbar.clock_well.size.h,
        },
        icon: None,
        label: None,
        tooltip: None,
        action: Some(UiAction::OpenPanel(PanelKind::Calendar)),
        accessible: Some(AccessibleInfo::new(
            AccessibleRole::Button,
            "Calendar and clock",
        )),
        visible: true,
        enabled: true,
        hovered: false,
        active: false,
        selected: false,
        hover_scale: 1.03,
        press_scale: 0.98,
    });
    ui.reconcile_focus();
}

#[cfg(test)]
mod tests {
    use super::hdr_tooltip;
    use crate::atlas::IconId;
    use crate::element::{ChromeItem, UiElement};
    use crate::ui_builder::{
        SIDEBAR_BROWSER_ID, SIDEBAR_DELETE_WORKSPACE_ID, SIDEBAR_EMAIL_ID, SIDEBAR_FILES_ID,
        SIDEBAR_TERMINAL_ID, SIDEBAR_WORKSPACE_OVERFLOW_ID, TOPBAR_FLOW_FIELD_ID, UiAction,
        UiBuildOptions, VoiceCaptureStatus, build_ui_for_output_with_options, default_status_items,
        sidebar_workspace_id, sidebar_workspace_number,
    };
    use crate::uitree::UiTree;

    #[test]
    fn constrained_layouts_expose_overflow_controls() {
        let options = UiBuildOptions {
            status_items: Some(
                (0..6)
                    .map(|index| {
                        ChromeItem::new(
                            800 + index,
                            IconId::Power,
                            format!("Status {index}"),
                            UiAction::Custom(800 + index),
                        )
                    })
                    .collect(),
            ),
            sidebar_items: Some(
                (0..4)
                    .map(|index| {
                        ChromeItem::new(
                            850 + index,
                            IconId::Browser,
                            format!("Sidebar {index}"),
                            UiAction::Custom(850 + index),
                        )
                    })
                    .collect(),
            ),
            ..UiBuildOptions::default()
        };
        let layout = crate::chrome_layout::build_chrome_layout_with_config(
            smithay::utils::Size::from((420, 190)),
            64,
            76,
            options.layout_config(),
        );
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, options);

        assert!(
            ui.elements
                .iter()
                .any(|element| element.id == crate::topbar::TopBar::OVERFLOW_ID)
        );
        assert!(
            ui.elements
                .iter()
                .any(|element| element.id == crate::sidebar::SideBar::OVERFLOW_ID)
        );
    }

    #[test]
    fn default_status_collection_gets_one_well_per_visible_item() {
        let options = UiBuildOptions::default();
        let visible_count = default_status_items(&options)
            .iter()
            .filter(|item| item.visible)
            .count();
        let layout = crate::chrome_layout::build_chrome_layout_with_config(
            smithay::utils::Size::from((1920, 1080)),
            64,
            76,
            options.layout_config(),
        );

        assert_eq!(layout.topbar.status_wells.len(), visible_count);

        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, options);
        assert_eq!(
            ui.elements
                .iter()
                .filter(|element| { element.kind == crate::types::UiElementKind::TopbarIndicator })
                .count(),
            visible_count
        );
        assert!(
            !ui.elements
                .iter()
                .any(|element| element.id == crate::topbar::TopBar::OVERFLOW_ID)
        );
    }

    #[test]
    fn replacement_collections_control_order_visibility_and_actions() {
        let options = UiBuildOptions {
            status_items: Some(vec![
                ChromeItem::new(900, IconId::Power, "First visible", UiAction::Custom(900)),
                ChromeItem::new(901, IconId::Bluetooth, "Hidden", UiAction::Custom(901))
                    .visible(false),
                ChromeItem::new(902, IconId::Wifi, "Second visible", UiAction::Custom(902))
                    .active(true),
            ]),
            sidebar_items: Some(vec![ChromeItem::new(
                903,
                IconId::Browser,
                "Runtime browser",
                UiAction::LaunchApp("runtime-browser".into()),
            )]),
            ..UiBuildOptions::default()
        };
        let config = options.layout_config();
        assert_eq!(config.status_item_count, 2);
        assert_eq!(config.sidebar_item_count, 1);

        let layout = crate::chrome_layout::build_chrome_layout_with_config(
            smithay::utils::Size::from((1920, 1080)),
            64,
            76,
            config,
        );
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, options);

        let status_ids: Vec<_> = ui
            .elements
            .iter()
            .filter(|element| element.kind == crate::types::UiElementKind::TopbarIndicator)
            .map(|element| element.id)
            .collect();
        assert_eq!(status_ids, vec![900, 902]);
        assert!(ui.elements.iter().any(|element| {
            element.id == 902
                && element.active
                && matches!(element.action, Some(UiAction::Custom(902)))
        }));
        assert!(ui.elements.iter().any(|element| {
            element.id == 903
                && matches!(element.action, Some(UiAction::LaunchApp(ref command)) if command == "runtime-browser")
        }));
    }

    fn build(workspace_count: usize, max_workspace_slots: usize) -> UiTree {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let layout = crate::chrome_layout::build_chrome_layout(output_size, 64, 76);
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(
            &mut ui,
            &layout,
            UiBuildOptions {
                workspace_count,
                max_workspace_slots,
                ..UiBuildOptions::default()
            },
        );
        ui
    }

    #[test]
    fn workspace_id_round_trips() {
        for n in 1..=9u32 {
            assert_eq!(sidebar_workspace_number(sidebar_workspace_id(n)), Some(n));
        }
        assert_eq!(sidebar_workspace_number(SIDEBAR_BROWSER_ID), None);
    }

    #[test]
    fn sidebar_shows_all_workspaces_within_slot_cap_without_overflow() {
        let ui = build(2, 4);

        let workspace_ids: Vec<u32> = ui
            .elements
            .iter()
            .filter_map(|el| sidebar_workspace_number(el.id))
            .collect();
        assert_eq!(workspace_ids, vec![1, 2]);
        assert!(
            ui.elements
                .iter()
                .any(|el| el.id == SIDEBAR_DELETE_WORKSPACE_ID)
        );
        assert!(
            !ui.elements
                .iter()
                .any(|el| el.id == SIDEBAR_WORKSPACE_OVERFLOW_ID)
        );
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_BROWSER_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_TERMINAL_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_FILES_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_EMAIL_ID));
        let files_index = ui
            .elements
            .iter()
            .position(|el| el.id == SIDEBAR_FILES_ID)
            .unwrap();
        let email = &ui.elements[files_index + 1];
        assert_eq!(email.id, SIDEBAR_EMAIL_ID);
        assert_eq!(email.icon, Some(IconId::Email));
        assert!(matches!(
            email.action,
            Some(UiAction::Custom(SIDEBAR_EMAIL_ID))
        ));
    }

    #[test]
    fn default_sidebar_buttons_all_have_actions_after_a_rebuild() {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let layout = crate::chrome_layout::build_chrome_layout(output_size, 64, 76);
        let mut ui = UiTree::default();

        for _ in 0..2 {
            build_ui_for_output_with_options(&mut ui, &layout, UiBuildOptions::default());
            let sidebar_buttons: Vec<_> = ui
                .elements
                .iter()
                .filter(|element| {
                    element.kind == crate::types::UiElementKind::SidebarButton && element.visible
                })
                .collect();
            assert!(!sidebar_buttons.is_empty());
            assert!(
                sidebar_buttons
                    .iter()
                    .all(|element| element.enabled && element.action.is_some())
            );
        }
    }

    #[test]
    fn sidebar_collapses_to_overflow_once_workspace_count_exceeds_slot_setting() {
        let ui = build(5, 3);

        let workspace_ids: Vec<u32> = ui
            .elements
            .iter()
            .filter_map(|el| sidebar_workspace_number(el.id))
            .collect();
        assert_eq!(workspace_ids, vec![1, 2, 3]);
        assert!(
            ui.elements
                .iter()
                .any(|el| el.id == SIDEBAR_WORKSPACE_OVERFLOW_ID)
        );
        // Fixed buttons must never be pushed out even when workspaces overflow.
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_BROWSER_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_TERMINAL_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_FILES_ID));
        assert!(ui.elements.iter().any(|el| el.id == SIDEBAR_EMAIL_ID));
    }

    #[test]
    fn hdr_tooltip_reflects_request_vs_kms_state() {
        assert_eq!(hdr_tooltip(true, true, true), "HDR active (KMS live)");
        assert_eq!(
            hdr_tooltip(true, true, false),
            "HDR requested (pending KMS)"
        );
        assert_eq!(hdr_tooltip(true, false, false), "HDR supported (off)");
        assert_eq!(hdr_tooltip(false, true, true), "HDR not detected");
    }

    #[test]
    fn topbar_flow_field_launches_ai_console_directly() {
        let flow_field = build_flow_field();
        assert!(matches!(
            flow_field.action.as_ref(),
            Some(UiAction::LaunchApp(command)) if command == "focaldesk-ai-console"
        ));
        assert_eq!(flow_field.icon, Some(IconId::AiConsole));
    }

    #[test]
    fn audio_well_shows_voice_capture_state() {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let layout = crate::chrome_layout::build_chrome_layout(output_size, 64, 76);
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(
            &mut ui,
            &layout,
            UiBuildOptions {
                microphone_detected: true,
                voice_capture_status: VoiceCaptureStatus::Listening,
                ..UiBuildOptions::default()
            },
        );

        let microphone = ui
            .elements
            .iter()
            .find(|element| element.icon == Some(IconId::Microphone))
            .expect("microphone indicator");
        assert_eq!(
            microphone.tooltip.as_deref(),
            Some("Voice input: listening — Super+Shift+V to stop")
        );
        assert!(microphone.selected);
        assert!(microphone.active);
        assert!(matches!(
            microphone.action,
            Some(UiAction::OpenPanel(crate::types::PanelKind::Audio))
        ));

        build_ui_for_output_with_options(
            &mut ui,
            &layout,
            UiBuildOptions {
                microphone_detected: true,
                voice_capture_status: VoiceCaptureStatus::Idle,
                ..UiBuildOptions::default()
            },
        );
        let microphone_off = ui
            .elements
            .iter()
            .find(|element| element.icon == Some(IconId::MicrophoneOff))
            .expect("idle microphone indicator");
        assert_eq!(
            microphone_off.tooltip.as_deref(),
            Some("Voice input: not listening — Super+Shift+V to start")
        );
        assert!(!microphone_off.selected);
        assert!(!microphone_off.active);
    }

    #[test]
    fn camera_well_distinguishes_presence_and_active_use() {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let idle_options = UiBuildOptions {
            camera_detected: true,
            camera_active: false,
            ..UiBuildOptions::default()
        };
        let layout = crate::chrome_layout::build_chrome_layout_with_config(
            output_size,
            64,
            76,
            idle_options.layout_config(),
        );
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, idle_options);

        let idle_camera = ui
            .elements
            .iter()
            .find(|element| element.id == super::TOPBAR_CAMERA_ID)
            .expect("camera indicator");
        assert_eq!(idle_camera.icon, Some(IconId::VideoOff));
        assert_eq!(
            idle_camera.tooltip.as_deref(),
            Some("Camera detected — not in use")
        );
        assert!(idle_camera.selected);
        assert!(!idle_camera.active);

        build_ui_for_output_with_options(
            &mut ui,
            &layout,
            UiBuildOptions {
                camera_detected: true,
                camera_active: true,
                ..UiBuildOptions::default()
            },
        );
        let active_camera = ui
            .elements
            .iter()
            .find(|element| element.id == super::TOPBAR_CAMERA_ID)
            .expect("active camera indicator");
        assert_eq!(active_camera.icon, Some(IconId::Video));
        assert_eq!(active_camera.tooltip.as_deref(), Some("Camera in use"));
        assert!(active_camera.selected);
        assert!(active_camera.active);
    }

    fn build_flow_field() -> UiElement {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let layout = crate::chrome_layout::build_chrome_layout(output_size, 64, 76);
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, UiBuildOptions::default());
        ui.elements
            .iter()
            .find(|el| el.id == TOPBAR_FLOW_FIELD_ID)
            .cloned()
            .expect("flow field")
    }
}
