use crate::{FlowAction, FlowEvent, WindowId};

use std::collections::HashMap; // or whatever you use
use std::collections::HashSet;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct OutputId(pub u64);



pub const PIN_SLOTS: usize = 9;

#[derive(Debug, Clone)]
pub struct OutputPins {
    pub slots: [Option<WindowId>; PIN_SLOTS],
    pub active_slot: Option<u8>, // 1..=9, or None
}

impl Default for OutputPins {
    fn default() -> Self {
        Self {
            slots: [None; PIN_SLOTS],
            active_slot: None,
        }
    }
}

pub struct FlowState {
    windows: Vec<WindowId>,
    known: HashSet<WindowId>,
    pub focused: Option<WindowId>,
    pub launcher_open: bool,
    next_window_id: WindowId,
    pub mru: Vec<WindowId>,
    pub pins_by_output: HashMap<OutputId, OutputPins>, // slots + selected glow
}

impl Default for FlowState {
    fn default() -> Self {
        Self {
            next_window_id: 1,
            focused: None,
            launcher_open: false,
            known: HashSet::new(),
            windows: Vec::new(),
            mru: Vec::new(),
            pins_by_output: HashMap::new(),
        }
    }
}

impl FlowState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mru_vec(&self) -> &[WindowId] {
        &self.mru
    }
    pub fn mark_known(&mut self, wid: WindowId) {
        if !self.known.contains(&wid) {
            self.known.insert(wid);
        }
    }
    pub fn note_focus(&mut self, wid: WindowId) {
        self.focused = Some(wid);
        self.mru.retain(|&x| x != wid); // dedup    
        self.mru.push(wid); // newest at end
        // If you want newest at front instead:
        // self.mru.insert(0, wid);
    }
    pub fn mru_len(&self) -> usize {
        self.mru.len()
    }

    pub fn remove_window(&mut self, wid: WindowId) {
        // remove from MRU
        self.mru.retain(|&x| x != wid);
        self.known.remove(&wid);
        // clear focus if it was focused
        if self.focused == Some(wid) {
            self.focused = None;
        }

        // remove from known set if you have one
        self.known.retain(|&x| x != wid);
    }

    pub fn most_recent(&self) -> Option<WindowId> {
        self.mru.last().copied()
    }
    pub fn top(&self) -> Option<WindowId> {
        self.windows.last().copied()
    }
    
    pub fn windows(&self) -> &[WindowId] {
        &self.windows
    }
    
    fn raise(&mut self, id: WindowId) {
        if let Some(pos) = self.windows.iter().position(|&w| w == id) {
            self.windows.remove(pos);
        }
        self.windows.push(id);
    }
    
    pub fn alloc_window_id(&mut self) -> WindowId {
        let id = self.next_window_id;
        self.next_window_id += 1;
        id
    }
    
    pub fn launcher_open(&self) -> bool {
      self.launcher_open
    } 
    
    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }
    
    pub fn set_focused(&mut self, wid: WindowId) {
        // optional guard if you want:
        // if self.known.contains(&wid) {
        self.focused = Some(wid);
        self.raise(wid);
    }

    pub fn handle(&mut self, event: FlowEvent) -> FlowAction {
        match event {
        FlowEvent::WindowMapped { id } => {
            self.raise(id);
            self.focused = Some(id);
            FlowAction::Focus(id)
        }


        FlowEvent::WindowUnmapped { id } => {
            let before = self.windows.len();
            self.windows.retain(|&w| w != id);
            if self.windows.len() == before {
                return FlowAction::None;
            }

            if self.focused == Some(id) {
                self.focused = self.windows.last().copied();
            if let Some(new_id) = self.focused {
                FlowAction::Focus(new_id)
            } else {
                FlowAction::None
        }
            } else {
                FlowAction::None
            }
        }
        
        
        
            FlowEvent::FocusChanged { id } => {
                if self.focused == id {
                    return FlowAction::None;
                }

                self.focused = id;

                if let Some(wid) = id {
                    // optional but recommended if you're using MRU ordering:
                    self.raise(wid);
                    FlowAction::Focus(wid)
                } else {
                    FlowAction::None
                }
            }

            FlowEvent::CloseFocused => match self.focused {
                Some(id) => FlowAction::Close(id),
                None => FlowAction::None,
            }

            FlowEvent::FocusNext => {
                if self.windows.is_empty() {
                    self.focused = None;
                    FlowAction::None
                } else {
                    let next = match self.focused {
                        None => self.windows[0],
                        Some(cur) => {
                            let i = self.windows.iter().position(|&x| x == cur).unwrap_or(0);
                            self.windows[(i + 1) % self.windows.len()]
                        }
                    };
                    self.focused = Some(next);
                    FlowAction::Focus(next)
                }
            }
            
            FlowEvent::Key { combo } => {
            if combo == "Mod+Shift+Q" {
                if let Some(id) = self.focused {
                  FlowAction::Close(id)
                }
                else {
                  FlowAction::None
                }
            } else if combo == "Mod+Q" {
                    FlowAction::Quit
                } else if combo == "Mod+Space" { 
                    self.launcher_open = !self.launcher_open;
                    FlowAction::ToggleLauncher 
                } else if combo == "Mod+Enter" {
                    FlowAction::Spawn {
                      cmd: "weston-terminal".into(),
                      args: vec![],
                    }
                } else {
                    FlowAction::None
                }
            }

            _ => FlowAction::None,
        }
    }
}
