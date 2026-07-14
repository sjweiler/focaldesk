use crate::atlas::IconId;
use crate::chrome_layout::ChromeLayout;
use crate::element::UiElement;
use crate::element::UiRect;
use crate::types::{PanelKind, UiAction, UiElementKind};
use crate::uitree::UiTree;

const SIDEBAR_BASE: u32 = 1_000;
const CLOCK_ID: u32 = 100_000;
pub const TOPBAR_FLOW_FIELD_ID: u32 = 100_001;
pub const SIDEBAR_SETTINGS_ID: u32 = SIDEBAR_BASE + 1;
pub const SIDEBAR_ADD_WORKSPACE_ID: u32 = SIDEBAR_BASE + 3;
pub const SIDEBAR_DELETE_WORKSPACE_ID: u32 = SIDEBAR_BASE + 4;
pub const SIDEBAR_BROWSER_ID: u32 = SIDEBAR_BASE + 5;
pub const SIDEBAR_TERMINAL_ID: u32 = SIDEBAR_BASE + 6;
pub const SIDEBAR_FILES_ID: u32 = SIDEBAR_BASE + 7;
pub const SIDEBAR_WORKSPACE_OVERFLOW_ID: u32 = SIDEBAR_BASE + 10;

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

#[derive(Debug, Clone, Copy)]
pub struct UiBuildOptions {
    pub hdr_supported: bool,
    pub hdr_requested: bool,
    pub hdr_kms_applied: bool,
    pub workspace_count: usize,
    /// Max number of workspace buttons shown individually before they
    /// collapse into an overflow button. Mirrors
    /// `WorkspaceSettings::max_workspace_slots`.
    pub max_workspace_slots: usize,
    pub active_workspace: u32,
    pub ai_flow_mode: AiFlowMode,
}

impl Default for UiBuildOptions {
    fn default() -> Self {
        Self {
            hdr_supported: false,
            hdr_requested: false,
            hdr_kms_applied: false,
            workspace_count: 1,
            max_workspace_slots: 4,
            active_workspace: 1,
            ai_flow_mode: AiFlowMode::Idle,
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

pub fn build_ui_for_output(ui: &mut UiTree, layout: &ChromeLayout) {
    build_ui_for_output_with_options(ui, layout, UiBuildOptions::default());
}

pub fn build_ui_for_output_with_options(
    ui: &mut UiTree,
    layout: &ChromeLayout,
    options: UiBuildOptions,
) {
    ui.elements.clear();

    let workspace_count = options.workspace_count.max(1);
    let (flow_selected, flow_active, flow_enabled) = match options.ai_flow_mode {
        AiFlowMode::Idle => (false, false, true),
        AiFlowMode::Thinking => (true, false, true),
        AiFlowMode::Acting => (false, true, true),
        AiFlowMode::PermissionWait => (true, true, true),
        AiFlowMode::Error => (false, false, false),
    };

    let mut sidebar_entries = vec![
        (
            SIDEBAR_SETTINGS_ID,
            IconId::Settings,
            "Settings".to_string(),
            UiAction::Custom(SIDEBAR_SETTINGS_ID),
            false,
        ),
        (
            SIDEBAR_BASE,
            IconId::Launcher,
            "Launcher".to_string(),
            UiAction::LaunchApp("@launcher"),
            false,
        ),
    ];

    // Reserve one sidebar slot each for the buttons that always surround the
    // workspace buttons, so Browser/Terminal/Files can never be pushed off
    // the bottom of the sidebar by a high max_workspace_slots setting on a
    // short screen.
    let total_slots = layout.sidebar.slots.len();
    let fixed_before = 2; // settings, launcher
    let add_slot = 1;
    let remove_slot = if workspace_count > 1 { 1 } else { 0 };
    let fixed_after = 3; // browser, terminal, files
    let reserved_no_overflow = fixed_before + add_slot + remove_slot + fixed_after;
    let slots_for_workspaces_no_overflow = total_slots.saturating_sub(reserved_no_overflow).max(1);

    // Clamp to 9: the icon atlas only defines Slot(1)..=Slot(9), regardless of
    // what the settings file says.
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

    for workspace in 1..=displayed_workspace_count {
        let id = sidebar_workspace_id(workspace as u32);
        sidebar_entries.push((
            id,
            IconId::Slot(workspace as u8),
            format!("Workspace {workspace}"),
            UiAction::Custom(id),
            options.active_workspace == workspace as u32,
        ));
    }

    sidebar_entries.push((
        SIDEBAR_ADD_WORKSPACE_ID,
        IconId::Plus,
        "Add new workspace".to_string(),
        UiAction::Custom(SIDEBAR_ADD_WORKSPACE_ID),
        false,
    ));

    if workspace_count > 1 {
        sidebar_entries.push((
            SIDEBAR_DELETE_WORKSPACE_ID,
            IconId::Minus,
            "Delete workspace".to_string(),
            UiAction::Custom(SIDEBAR_DELETE_WORKSPACE_ID),
            false,
        ));
    }

    if show_overflow {
        sidebar_entries.push((
            SIDEBAR_WORKSPACE_OVERFLOW_ID,
            IconId::Overflow,
            "More workspaces".to_string(),
            UiAction::OpenPanel(PanelKind::Workspaces),
            options.active_workspace > displayed_workspace_count as u32,
        ));
    }

    sidebar_entries.extend([
        (
            SIDEBAR_BROWSER_ID,
            IconId::Browser,
            "Browser".to_string(),
            UiAction::Custom(SIDEBAR_BROWSER_ID),
            false,
        ),
        (
            SIDEBAR_TERMINAL_ID,
            IconId::Terminal,
            "Terminal".to_string(),
            UiAction::Custom(SIDEBAR_TERMINAL_ID),
            false,
        ),
        (
            SIDEBAR_FILES_ID,
            IconId::Files,
            "Files".to_string(),
            UiAction::Custom(SIDEBAR_FILES_ID),
            false,
        ),
    ]);

    for (slot, (id, icon, tooltip, action, selected)) in
        layout.sidebar.slots.iter().zip(sidebar_entries.into_iter())
    {
        let rect = slot.outer;

        let mut el = UiElement::sidebar_button(id, icon, tooltip, action);
        el.hover_scale = 1.10;
        el.press_scale = 0.96;
        el.bounds = UiRect {
            x: rect.loc.x,
            y: rect.loc.y,
            w: rect.size.w,
            h: rect.size.h,
        };
        el.selected = selected;
        ui.elements.push(el);
    }

    let mut flow_field = UiElement::topbar_indicator(
        TOPBAR_FLOW_FIELD_ID,
        IconId::AiConsole,
        "Launch FocalDesk AI Console",
    );
    flow_field.kind = UiElementKind::TopbarFlowField;
    flow_field.action = Some(UiAction::LaunchApp("focaldesk-ai-console"));
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

    for (i, rect) in layout.topbar.status_wells.iter().enumerate() {
        let (icon, tooltip, action) = match i {
            0 => (
                IconId::Wifi,
                "Network",
                UiAction::OpenPanel(PanelKind::Network),
            ),
            1 => (
                IconId::Bluetooth,
                "Bluetooth",
                UiAction::OpenPanel(PanelKind::Bluetooth),
            ),
            2 => (
                IconId::Speaker,
                "Audio",
                UiAction::OpenPanel(PanelKind::Audio),
            ),
            3 => (
                IconId::HDR,
                hdr_tooltip(
                    options.hdr_supported,
                    options.hdr_requested,
                    options.hdr_kms_applied,
                ),
                UiAction::OpenPanel(PanelKind::Display),
            ),
            4 => (
                IconId::Power,
                "Power menu",
                UiAction::OpenPanel(PanelKind::Power),
            ),
            _ => continue,
        };

        let mut el = UiElement::topbar_indicator(100 + i as u32, icon, tooltip);
        if icon == IconId::HDR {
            el.selected = options.hdr_kms_applied;
            el.active = options.hdr_requested && !options.hdr_kms_applied;
        }
        el.action = Some(action);
        el.hover_scale = 1.08;
        el.press_scale = 0.96;
        el.bounds = UiRect {
            x: rect.loc.x,
            y: rect.loc.y,
            w: rect.size.w,
            h: rect.size.h,
        };
        ui.elements.push(el);
    }

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
        visible: true,
        enabled: true,
        hovered: false,
        active: false,
        selected: false,
        hover_scale: 1.03,
        press_scale: 0.98,
    });
}

#[cfg(test)]
mod tests {
    use super::hdr_tooltip;
    use crate::atlas::IconId;
    use crate::element::UiElement;
    use crate::ui_builder::{
        SIDEBAR_BROWSER_ID, SIDEBAR_DELETE_WORKSPACE_ID, SIDEBAR_FILES_ID, SIDEBAR_TERMINAL_ID,
        SIDEBAR_WORKSPACE_OVERFLOW_ID, TOPBAR_FLOW_FIELD_ID, UiAction, UiBuildOptions,
        build_ui_for_output_with_options, sidebar_workspace_id, sidebar_workspace_number,
    };
    use crate::uitree::UiTree;

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
            Some(UiAction::LaunchApp("focaldesk-ai-console"))
        ));
        assert_eq!(flow_field.icon, Some(IconId::AiConsole));
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
