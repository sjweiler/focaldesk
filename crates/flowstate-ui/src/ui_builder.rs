use crate::atlas::IconId;
use crate::chrome_layout::ChromeLayout;
use crate::element::UiRect;
use crate::element::UiElement;
use crate::types::{PanelKind, UiAction, UiElementKind};
use crate::uitree::UiTree;

const SIDEBAR_BASE: u32 = 1_000;
const CLOCK_ID: u32 = 100_000;

pub fn build_ui_for_output(ui: &mut UiTree, layout: &ChromeLayout) {
    ui.elements.clear();

    for (i, slot) in layout.sidebar.slots.iter().enumerate() {
        let id = SIDEBAR_BASE + i as u32;
        let rect = slot.outer;

        let icon = match i {
            0 => IconId::Launcher,
            1 => IconId::Settings,
            2 => IconId::Slot(1),
            3 => IconId::Plus,
            4 => IconId::Browser,
            5 => IconId::Terminal,
            6 => IconId::Files,
            _ => continue,
        };

        let action = match i {
            0 => UiAction::OpenPanel(PanelKind::AppLauncher),
            1 => UiAction::OpenPanel(PanelKind::Settings),
            2 => UiAction::Custom(SIDEBAR_BASE + i as u32),
            3 => UiAction::Custom(SIDEBAR_BASE + i as u32),
            4 => UiAction::LaunchApp("chrome"),
            5 => UiAction::LaunchApp("weston-terminal"),
            6 => UiAction::LaunchApp("nautilus"),
            _ => continue,
        };

        let mut el = UiElement::sidebar_button(id, icon, "some tooltip", action);
        el.hover_scale = 1.10;
        el.press_scale = 0.96;
        el.bounds = UiRect {
            x: rect.loc.x,
            y: rect.loc.y,
            w: rect.size.w,
            h: rect.size.h,
        };
        if i == 2 {
            el.selected = true;
        }
        ui.elements.push(el);
    }

    for (i, rect) in layout.topbar.status_wells.iter().enumerate() {
        let (icon, action) = match i {
            0 => (IconId::Wifi, UiAction::OpenPanel(PanelKind::Network)),
            1 => (IconId::Bluetooth, UiAction::OpenPanel(PanelKind::Bluetooth)),
            2 => (IconId::Speaker, UiAction::OpenPanel(PanelKind::Audio)),
            3 => (IconId::Power, UiAction::OpenPanel(PanelKind::Power)),
            _ => continue,
        };

        let mut el = UiElement::topbar_indicator(100 + i as u32, icon, "tooltip");
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
        tooltip: Some("Clock / Calendar".into()),
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
