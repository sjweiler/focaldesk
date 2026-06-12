use crate::uitree::UiTree;
use crate::element::UiElement;
use crate::types::UiElementKind;
use crate::element::UiRect;
use crate::atlas::IconId;
use crate::types::UiAction;
use crate::types::PanelKind;



fn build_ui_for_output(ui: &mut UiTree, layout: &ChromeLayout) {
    ui.elements.clear();

    // Sidebar buttons
    for (i, rect) in layout.slot_outer_rects.iter().enumerate() {
        ui.elements.push(UiElement {
            id: i as u32,
            kind: UiElementKind::SidebarButton,
            bounds: UiRect {
                x: rect.loc.x,
                y: rect.loc.y,
                w: rect.size.w,
                h: rect.size.h,
            },
            icon: Some(IconId::Launcher), // or per-slot
            tooltip: Some(format!("Workspace {}", i + 1)),
            action: Some(UiAction::Custom(i as u32)),
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            label: None,
        });
    }

    // Topbar indicators
    for (i, rect) in layout.status_wells.iter().enumerate() {
        ui.elements.push(UiElement {
            id: 100 + i as u32,
            kind: UiElementKind::TopbarIndicator,
            bounds: UiRect {
                x: rect.loc.x,
                y: rect.loc.y,
                w: rect.size.w,
                h: rect.size.h,
            },
            icon: Some(IconId::Wifi),
            tooltip: Some("Network".into()),
            action: Some(UiAction::OpenPanel(PanelKind::Network)),
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            label: None,
        });
    }
}
