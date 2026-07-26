use crate::UiVisualState;
use crate::accessibility::{AccessibleInfo, AccessibleRole};
use crate::atlas::IconId;
use crate::types::{ElementId, UiAction, UiElementKind};
use smithay::utils::Logical;
use smithay::utils::Rectangle;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl UiRect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

impl From<Rectangle<i32, Logical>> for UiRect {
    fn from(rect: Rectangle<i32, Logical>) -> Self {
        Self {
            x: rect.loc.x,
            y: rect.loc.y,
            w: rect.size.w,
            h: rect.size.h,
        }
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
    pub accessible: Option<AccessibleInfo>,
    pub visible: bool,
    pub enabled: bool,
    pub hovered: bool,
    pub active: bool,
    pub selected: bool,
    pub hover_scale: f32, // e.g. 1.10
    pub press_scale: f32, // e.g. 0.96
}

/// Declarative chrome content before a topbar/sidebar layout assigns bounds.
///
/// Runtime owners can add, remove, reorder, hide, or update these items without
/// coupling their behavior to a slot index. [`UiElement`] remains the laid-out,
/// interactive representation used for hit testing and rendering.
#[derive(Debug, Clone)]
pub struct ChromeItem {
    pub id: ElementId,
    pub icon: IconId,
    pub tooltip: String,
    pub action: UiAction,
    pub visible: bool,
    pub enabled: bool,
    pub active: bool,
    pub selected: bool,
}

impl ChromeItem {
    pub fn new(id: ElementId, icon: IconId, tooltip: impl Into<String>, action: UiAction) -> Self {
        Self {
            id,
            icon,
            tooltip: tooltip.into(),
            action,
            visible: true,
            enabled: true,
            active: false,
            selected: false,
        }
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl UiElement {
    pub fn from_chrome_item(kind: UiElementKind, item: ChromeItem, bounds: UiRect) -> Self {
        let accessible_role = match kind {
            UiElementKind::WorkspaceSlot => AccessibleRole::Tab,
            _ => AccessibleRole::Button,
        };
        let accessible = AccessibleInfo::new(accessible_role, item.tooltip.clone());
        Self {
            id: item.id,
            kind,
            bounds,
            icon: Some(item.icon),
            label: None,
            tooltip: Some(item.tooltip),
            action: Some(item.action),
            accessible: Some(accessible),
            visible: item.visible,
            enabled: item.enabled,
            hovered: false,
            active: item.active,
            selected: item.selected,
            hover_scale: 1.1,
            press_scale: 0.96,
        }
    }

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
        _bounds: UiRect,
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
            accessible: None,
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
        let tooltip = tooltip.into();
        Self {
            id,
            kind: UiElementKind::SidebarButton,
            bounds: UiRect::default(),
            icon: Some(icon),
            label: None,
            tooltip: Some(tooltip.clone()),
            action: Some(action),
            accessible: Some(AccessibleInfo::new(AccessibleRole::Button, tooltip)),
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            selected: false,
            hover_scale: 1.10,
            press_scale: 0.96,
        }
    }

    pub fn topbar_indicator(id: ElementId, icon: IconId, tooltip: impl Into<String>) -> Self {
        let tooltip = tooltip.into();
        Self {
            id,
            kind: UiElementKind::TopbarIndicator,
            bounds: UiRect::default(),
            icon: Some(icon),
            label: None,
            tooltip: Some(tooltip.clone()),
            action: None,
            accessible: Some(AccessibleInfo::new(AccessibleRole::Status, tooltip).live(true)),
            visible: true,
            enabled: true,
            hovered: false,
            active: false,
            selected: false,
            hover_scale: 1.08,
            press_scale: 0.96,
        }
    }

    /// Overrides the inferred accessibility semantics for this element.
    pub fn with_accessible(mut self, accessible: AccessibleInfo) -> Self {
        self.accessible = Some(accessible);
        self
    }

    /// Returns true when assistive technology can move focus to this element.
    pub fn is_accessibility_focusable(&self) -> bool {
        self.visible
            && self.enabled
            && self.action.is_some()
            && self
                .accessible
                .as_ref()
                .is_some_and(|info| info.role.is_interactive())
    }
}
