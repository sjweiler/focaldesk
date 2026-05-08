use crate::atlas::IconId;
use crate::types::{ElementId, UiAction, UiElementKind};
use crate::UiVisualState;


#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl UiRect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.w
            && py >= self.y
            && py < self.y + self.h
    }
}


#[derive(Debug, Clone)]
pub struct UiElement {
    pub id: ElementId,
    pub kind: UiElementKind,
    pub bounds: UiRect,
    pub icon: Option<IconId>,
    pub label: Option<String>,
    pub tooltip: Option<String>,
    pub action: Option<UiAction>,
    pub visible: bool,
    pub enabled: bool,
    pub hovered: bool,
    pub active: bool,
    pub selected: bool,
    pub hover_scale: f32,   // e.g. 1.10
    pub press_scale: f32,   // e.g. 0.96
}

impl UiElement {
    pub fn visual_state(&self) -> UiVisualState {
        if !self.enabled {
            UiVisualState::Disabled
        } else if self.active {
            UiVisualState::Active
        } else if self.selected {
            UiVisualState::Selected
        } else if self.hovered {
            UiVisualState::Hover
        } else {
            UiVisualState::Inactive
        }
    }
    pub fn visual_state_for_element(el: &UiElement, selected: bool) -> UiVisualState {
    if !el.enabled {
        UiVisualState::Disabled
    } else if el.active {
        UiVisualState::Active
    } else if selected {
        UiVisualState::Selected
    } else if el.hovered {
        UiVisualState::Hover
    } else {
        UiVisualState::Inactive
    }
}
pub fn new(
        id: ElementId,
        bounds: UiRect,
        kind: UiElementKind,
        icon: Option<IconId>,
        action: Option<UiAction>,
    ) -> Self {
        Self {
            id,
            bounds: UiRect::default(),
            kind,
            icon,
            label: None,
            tooltip: None,
            action,
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            selected: false,
            hover_scale: 1.1,
            press_scale: 0.96,
        }
    }
    pub fn sidebar_button(
        id: ElementId,
        icon: IconId,
        tooltip: impl Into<String>,
        action: UiAction,
        
    ) -> Self {
        Self {
            id,
            kind: UiElementKind::SidebarButton,
            bounds: UiRect::default(),
            icon: Some(icon),
            label: None,
            tooltip: Some(tooltip.into()),
            action: Some(action),
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            selected: false,
            hover_scale: 1.10,
            press_scale: 0.96,        }
    }

    pub fn topbar_indicator(
        id: ElementId,
        icon: IconId,
        tooltip: impl Into<String>,
    ) -> Self {
        Self {
            id,
            kind: UiElementKind::TopbarIndicator,
            bounds: UiRect::default(),
            icon: Some(icon),
            label: None,
            tooltip: Some(tooltip.into()),
            action: None,
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            selected: false,
            hover_scale: 1.08,
            press_scale: 0.96,
        }
    }
}
