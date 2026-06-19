use crate::atlas::IconId;
use crate::chrome_layout::ChromeLayout;
use crate::element::UiElement;
use crate::element::UiRect;
use crate::types::{PanelKind, UiAction, UiElementKind};
use crate::uitree::UiTree;

const SIDEBAR_BASE: u32 = 1_000;
const CLOCK_ID: u32 = 100_000;
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

#[derive(Debug, Clone, Copy)]
pub struct UiBuildOptions {
    pub hdr_supported: bool,
    pub hdr_enabled: bool,
    pub workspace_count: usize,
    pub active_workspace: u32,
}

impl Default for UiBuildOptions {
    fn default() -> Self {
        Self {
            hdr_supported: false,
            hdr_enabled: false,
            workspace_count: 1,
            active_workspace: 1,
        }
    }
}

fn hdr_tooltip(hdr_supported: bool, hdr_enabled: bool) -> &'static str {
    if hdr_supported && hdr_enabled {
        "HDR supported (enabled)"
    } else if hdr_supported {
        "HDR supported (inactive)"
    } else {
        "HDR not detected"
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
            UiAction::OpenPanel(PanelKind::AppLauncher),
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
                hdr_tooltip(options.hdr_supported, options.hdr_enabled),
                UiAction::OpenPanel(PanelKind::Display),
            ),
            4 => (
                IconId::Power,
                "Power",
                UiAction::OpenPanel(PanelKind::Power),
            ),
            _ => continue,
        };

        let mut el = UiElement::topbar_indicator(100 + i as u32, icon, tooltip);
        if icon == IconId::HDR {
            el.selected = options.hdr_enabled;
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

    #[test]
    fn hdr_tooltip_only_reports_enabled_for_applied_supported_hdr() {
        assert_eq!(hdr_tooltip(true, true), "HDR supported (enabled)");
        assert_eq!(hdr_tooltip(true, false), "HDR supported (inactive)");
        assert_eq!(hdr_tooltip(false, true), "HDR not detected");
        assert_eq!(hdr_tooltip(false, false), "HDR not detected");
    }
}
