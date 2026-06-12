use focaldesk_types::{OutputId, WindowId};
use std::collections::HashMap;

/// Logical state of FlowOS.
/// Owns window ordering, focus, slot assignments, and outputs.
#[derive(Debug)]
pub struct FocalDesk {
    /// Slot assignments (slot index -> window)
    slots: [Option<WindowId>; 9],

    /// Outputs
    outputs: HashMap<OutputId, OutputInfo>,
}

#[derive(Debug)]
struct OutputInfo {
    pub active: bool,
}

impl FocalDesk {
    pub fn new() -> Self {
        Self {
            slots: [None; 9],
            outputs: HashMap::new(),
        }
    }

    /// Assign focused window to slot
    pub fn assign_slot(&mut self, slot: usize, focused: WindowId) {
        if slot >= self.slots.len() {
            return;
        }

        self.slots[slot] = Some(focused);
    }

    /// Activate slot
    pub fn activate_slot(&self, slot: usize) -> Option<WindowId> {
        self.slots[slot]
    }
}

impl Default for FocalDesk {
    fn default() -> Self {
        Self::new()
    }
}
