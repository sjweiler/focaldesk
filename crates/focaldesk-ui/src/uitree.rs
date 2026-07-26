use crate::element::UiElement;
use crate::types::{ElementId, UiAction};

#[derive(Default)]
pub struct UiTree {
    pub elements: Vec<UiElement>,
    pub hovered: Option<ElementId>,
    pub pressed: Option<ElementId>,
    /// Keyboard/assistive-technology focus within compositor-owned chrome.
    pub focused: Option<ElementId>,
}

impl UiTree {
    pub fn hit_test(&self, x: i32, y: i32) -> Option<&UiElement> {
        self.elements
            .iter()
            .rev()
            .find(|e| e.visible && e.bounds.contains(x, y))
    }

    pub fn hit_test_mut(&mut self, x: i32, y: i32) -> Option<&mut UiElement> {
        self.elements
            .iter_mut()
            .rev()
            .find(|e| e.visible && e.bounds.contains(x, y))
    }

    pub fn accessible_elements(&self) -> impl Iterator<Item = &UiElement> {
        self.elements
            .iter()
            .filter(|element| element.visible && element.accessible.is_some())
    }

    pub fn focused_element(&self) -> Option<&UiElement> {
        let focused = self.focused?;
        self.elements.iter().find(|element| element.id == focused)
    }

    pub fn focused_action(&self) -> Option<UiAction> {
        self.focused_element()
            .filter(|element| element.is_accessibility_focusable())
            .and_then(|element| element.action.clone())
    }

    pub fn set_focus(&mut self, id: ElementId) -> bool {
        if self
            .elements
            .iter()
            .any(|element| element.id == id && element.is_accessibility_focusable())
        {
            self.focused = Some(id);
            true
        } else {
            false
        }
    }

    pub fn clear_focus(&mut self) {
        self.focused = None;
    }

    pub fn focus_first(&mut self) -> Option<ElementId> {
        self.focused = self
            .elements
            .iter()
            .find(|element| element.is_accessibility_focusable())
            .map(|element| element.id);
        self.focused
    }

    /// Moves focus in semantic tree order and wraps at the end.
    pub fn focus_next(&mut self) -> Option<ElementId> {
        self.move_focus(1)
    }

    /// Moves focus in reverse semantic tree order and wraps at the beginning.
    pub fn focus_previous(&mut self) -> Option<ElementId> {
        self.move_focus(-1)
    }

    /// Keeps focus stable across UI rebuilds, clearing it if its element
    /// disappeared or became unavailable.
    pub fn reconcile_focus(&mut self) -> Option<ElementId> {
        if self.focused.is_some_and(|id| {
            self.elements
                .iter()
                .any(|element| element.id == id && element.is_accessibility_focusable())
        }) {
            self.focused
        } else {
            self.focused = None;
            None
        }
    }

    fn move_focus(&mut self, direction: isize) -> Option<ElementId> {
        let focusable: Vec<ElementId> = self
            .elements
            .iter()
            .filter(|element| element.is_accessibility_focusable())
            .map(|element| element.id)
            .collect();

        if focusable.is_empty() {
            self.focused = None;
            return None;
        }

        let next_index = self
            .focused
            .and_then(|focused| focusable.iter().position(|id| *id == focused))
            .map(|index| (index as isize + direction).rem_euclid(focusable.len() as isize) as usize)
            .unwrap_or_else(|| {
                if direction < 0 {
                    focusable.len() - 1
                } else {
                    0
                }
            });

        self.focused = Some(focusable[next_index]);
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::UiTree;
    use crate::accessibility::{AccessibleInfo, AccessibleRole};
    use crate::atlas::IconId;
    use crate::element::UiElement;
    use crate::types::{UiAction, UiElementKind};

    fn button(id: u32, name: &str) -> UiElement {
        UiElement::new(
            id,
            Default::default(),
            UiElementKind::SidebarButton,
            Some(IconId::Launcher),
            Some(UiAction::Custom(id)),
        )
        .with_accessible(AccessibleInfo::new(AccessibleRole::Button, name))
    }

    #[test]
    fn focus_traversal_skips_unavailable_and_noninteractive_elements() {
        let mut disabled = button(2, "Disabled");
        disabled.enabled = false;
        let status = UiElement::new(
            3,
            Default::default(),
            UiElementKind::OutputLabel,
            None,
            None,
        )
        .with_accessible(AccessibleInfo::new(AccessibleRole::Status, "Connected"));
        let mut hidden = button(4, "Hidden");
        hidden.visible = false;

        let mut tree = UiTree {
            elements: vec![
                button(1, "First"),
                disabled,
                status,
                hidden,
                button(5, "Last"),
            ],
            ..UiTree::default()
        };

        assert_eq!(tree.focus_next(), Some(1));
        assert_eq!(tree.focus_next(), Some(5));
        assert_eq!(tree.focus_next(), Some(1));
        assert_eq!(tree.focus_previous(), Some(5));
    }

    #[test]
    fn focused_action_and_rebuild_reconciliation_use_stable_ids() {
        let mut tree = UiTree {
            elements: vec![button(10, "Network"), button(20, "Power")],
            ..UiTree::default()
        };

        assert!(tree.set_focus(20));
        assert!(matches!(tree.focused_action(), Some(UiAction::Custom(20))));

        tree.elements = vec![button(20, "Power"), button(30, "Settings")];
        assert_eq!(tree.reconcile_focus(), Some(20));

        tree.elements[0].enabled = false;
        assert_eq!(tree.reconcile_focus(), None);
        assert!(tree.focused_action().is_none());
    }

    #[test]
    fn accessible_elements_exclude_hidden_visuals() {
        let mut hidden = button(2, "Hidden");
        hidden.visible = false;
        let tree = UiTree {
            elements: vec![button(1, "Visible"), hidden],
            ..UiTree::default()
        };

        let names: Vec<&str> = tree
            .accessible_elements()
            .filter_map(|element| element.accessible.as_ref())
            .map(|info| info.name.as_str())
            .collect();
        assert_eq!(names, vec!["Visible"]);
    }
}
