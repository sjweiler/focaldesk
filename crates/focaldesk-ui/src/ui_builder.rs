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
pub const SIDEBAR_WORKSPACE_1_ID: u32 = SIDEBAR_BASE + 2;
pub const SIDEBAR_ADD_WORKSPACE_ID: u32 = SIDEBAR_BASE + 3;
pub const SIDEBAR_DELETE_WORKSPACE_ID: u32 = SIDEBAR_BASE + 4;
pub const SIDEBAR_BROWSER_ID: u32 = SIDEBAR_BASE + 5;
pub const SIDEBAR_TERMINAL_ID: u32 = SIDEBAR_BASE + 6;
pub const SIDEBAR_FILES_ID: u32 = SIDEBAR_BASE + 7;
pub const SIDEBAR_WORKSPACE_2_ID: u32 = SIDEBAR_BASE + 8;
pub const SIDEBAR_WORKSPACE_3_ID: u32 = SIDEBAR_BASE + 9;
pub const SIDEBAR_WORKSPACE_OVERFLOW_ID: u32 = SIDEBAR_BASE + 10;

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

    for workspace in 1..=workspace_count.min(3) {
        let id = match workspace {
            1 => SIDEBAR_WORKSPACE_1_ID,
            2 => SIDEBAR_WORKSPACE_2_ID,
            3 => SIDEBAR_WORKSPACE_3_ID,
            _ => unreachable!(),
        };
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

    if workspace_count > 3 {
        sidebar_entries.push((
            SIDEBAR_WORKSPACE_OVERFLOW_ID,
            IconId::Overflow,
            "More workspaces".to_string(),
            UiAction::OpenPanel(PanelKind::Workspaces),
            options.active_workspace > 3,
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
        IconId::Launcher,
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
    use crate::ui_builder::{
        TOPBAR_FLOW_FIELD_ID, UiAction, UiBuildOptions, build_ui_for_output_with_options,
    };
    use crate::uitree::UiTree;

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
        let action = build_flow_field_action();
        assert!(matches!(
            action,
            UiAction::LaunchApp("focaldesk-ai-console")
        ));
    }

    fn build_flow_field_action() -> UiAction {
        let output_size = smithay::utils::Size::from((1920, 1080));
        let layout = crate::chrome_layout::build_chrome_layout(output_size, 64, 76);
        let mut ui = UiTree::default();
        build_ui_for_output_with_options(&mut ui, &layout, UiBuildOptions::default());
        ui.elements
            .iter()
            .find(|el| el.id == TOPBAR_FLOW_FIELD_ID)
            .and_then(|el| el.action.clone())
            .expect("flow field action")
    }
}
