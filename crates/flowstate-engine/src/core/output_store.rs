use std::collections::HashMap;

use flowstate_types::OutputId;
use smithay::output::Output;
use smithay::utils::Rectangle;

#[derive(Debug, Clone)]
pub struct OutputNode {
    pub id: OutputId,
    pub output: Output,
    pub name: String,
    pub logical_geometry: Rectangle<i32, smithay::utils::Logical>,
    pub enabled: bool,
}

#[derive(Debug, Default)]
pub struct OutputStore {
    next_id: u64,
    outputs: HashMap<OutputId, OutputNode>,
    order: Vec<OutputId>,
    primary: Option<OutputId>,
    focused: Option<OutputId>,
}

impl OutputStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        output: Output,
        name: String,
        logical_geometry: Rectangle<i32, smithay::utils::Logical>,
    ) -> OutputId {
        let id = OutputId(self.next_id);
        self.next_id += 1;

        let node = OutputNode {
            id,
            output,
            name,
            logical_geometry,
            enabled: true,
        };

        self.outputs.insert(id, node);
        self.order.push(id);

        if self.primary.is_none() {
            self.primary = Some(id);
        }
        if self.focused.is_none() {
            self.focused = Some(id);
        }

        id
    }

    pub fn remove(&mut self, id: OutputId) -> Option<OutputNode> {
        self.order.retain(|oid| *oid != id);

        if self.primary == Some(id) {
            self.primary = self.order.first().copied();
        }

        if self.focused == Some(id) {
            self.focused = self.order.first().copied();
        }

        self.outputs.remove(&id)
    }

    pub fn get(&self, id: OutputId) -> Option<&OutputNode> {
        self.outputs.get(&id)
    }

    pub fn get_mut(&mut self, id: OutputId) -> Option<&mut OutputNode> {
        self.outputs.get_mut(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (OutputId, &OutputNode)> {
        self.order
            .iter()
            .copied()
            .filter_map(|id| self.outputs.get(&id).map(|o| (id, o)))
    }

    pub fn primary(&self) -> Option<OutputId> {
        self.primary
    }

    pub fn focused(&self) -> Option<OutputId> {
        self.focused
    }

    pub fn set_focused(&mut self, id: OutputId) {
        if self.outputs.contains_key(&id) {
            self.focused = Some(id);
        }
    }
}
