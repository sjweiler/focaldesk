use crate::element::UiElement;
use crate::types::ElementId;


pub struct UiTree {
    pub elements: Vec<UiElement>,
    pub hovered: Option<ElementId>,
    pub pressed: Option<ElementId>,
}

impl UiTree {
    pub fn hit_test(&self, x: i32, y: i32) -> Option<&UiElement> {
        self.elements
            .iter()
            .rev()
            .find(|e| e.visible && e.enabled && e.bounds.contains(x, y))
    }

    pub fn hit_test_mut(&mut self, x: i32, y: i32) -> Option<&mut UiElement> {
        self.elements
            .iter_mut()
            .rev()
            .find(|e| e.visible && e.enabled && e.bounds.contains(x, y))
    }
}

impl Default for UiTree {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            hovered: None,
            pressed: None,
        }
    }
}
