use std::collections::HashMap;

use flowstate_types::WindowId;
use crate::core::shell::ManagedWindow;

#[derive(Default)]
pub struct WindowStore {
    pub by_id: HashMap<WindowId, ManagedWindow>,
    pub stacking: Vec<WindowId>,
    pub focused: Option<WindowId>,
}

impl WindowStore {
    pub fn insert(&mut self, window: ManagedWindow) {
        let id = window.id;
        self.by_id.insert(id, window);
        self.stacking.push(id);
    }

    pub fn remove(&mut self, id: WindowId) -> Option<ManagedWindow> {
        self.stacking.retain(|w| *w != id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.by_id.remove(&id)
    }

    pub fn get(&self, id: WindowId) -> Option<&ManagedWindow> {
        self.by_id.get(&id)
    }

    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut ManagedWindow> {
        self.by_id.get_mut(&id)
    }

    pub fn focus(&mut self, id: WindowId) {
        if let Some(old) = self.focused.take() {
            if let Some(w) = self.by_id.get_mut(&old) {
                w.set_activated(false);
            }
        }

        if let Some(w) = self.by_id.get_mut(&id) {
            w.set_activated(true);
            self.focused = Some(id);
        }

        self.raise(id);
    }

    pub fn raise(&mut self, id: WindowId) {
        self.stacking.retain(|w| *w != id);
        self.stacking.push(id);
    }

    pub fn iter_stacking(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.stacking.iter().filter_map(|id| self.by_id.get(id))
    }
}


