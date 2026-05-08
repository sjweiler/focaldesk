use crate::core::chrome_layout::ChromeLayout;
use flowstate_ui::uitree::UiTree;
use flowstate_ui::element::{UiElement, UiRect};
use flowstate_ui::types::{UiAction, UiElementKind, PanelKind};
use flowstate_ui::atlas::IconId;
const SIDEBAR_BASE: u32 = 1_000;
const TOPBAR_BASE: u32 = 2_000;
const WIDGET_BASE: u32 = 3_000;

const CLOCK_ID: u32 = 100_000;

pub fn build_ui_for_output(ui: &mut UiTree, layout: &ChromeLayout) {
    ui.elements.clear();

    for (i, rect) in layout.slot_outer_rects.iter().enumerate() {
        let id = SIDEBAR_BASE + i as u32;
    
        let icon = match i {
        0 => IconId::Launcher,
        1 => IconId::Settings,
        2 => IconId::Slot(1),
        3 => IconId::Plus,
       // 4 => IconId::Overflow,
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
            //4 => UiAction::OpenPanel(PanelKind::Workspaces),
            4 => UiAction::LaunchApp("chrome"),
            5 => UiAction::LaunchApp("weston-terminal"),
            6 => UiAction::LaunchApp("nautilus"),
            _ => continue,
        };
        
        let mut el = UiElement::sidebar_button(
            id,
            icon,
            "some tooltip",
            action,
        );

        el.hover_scale = 1.10;
        el.press_scale = 0.96;

        el.bounds = UiRect {
            x: rect.loc.x,
            y: rect.loc.y,
            w: rect.size.w,
            h: rect.size.h,
        };
        
        if i == 2 {
          el.selected = true
        }
        
        ui.elements.push(el);
    }

    for (i, rect) in layout.status_wells.iter().enumerate() {
           let (icon, tooltip, action) = match i {
                0 => (IconId::Wifi, "Network", UiAction::OpenPanel(PanelKind::Network)),
                1 => (IconId::Bluetooth, "Bluetooth", UiAction::OpenPanel(PanelKind::Bluetooth)),
                2 => (IconId::Speaker, "Audio", UiAction::OpenPanel(PanelKind::Audio)),
                3 => (IconId::Power, "Power", UiAction::OpenPanel(PanelKind::Power)),
                _ => continue,
            };
    
        
        let mut el = UiElement::topbar_indicator(
            100 + i as u32,
            icon,
            "tooltip",
        );

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
            x: layout.clock_well.loc.x,
            y: layout.clock_well.loc.y,
            w: layout.clock_well.size.w,
            h: layout.clock_well.size.h,
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
        hover_scale: 1.03,  // subtle
        press_scale: 0.98,  // very light press
    });
}
