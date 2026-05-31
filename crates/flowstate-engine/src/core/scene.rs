use crate::core::consts::MRU_CAP;
use crate::core::layout::LayoutSnapshot;
use flowstate_types::{OutputId, WindowId};
use indexmap::IndexMap;
use smithay::desktop::Space;
use smithay::desktop::Window;
use smithay::wayland::shell::xdg::ToplevelSurface;
use std::collections::HashMap;
use std::collections::HashSet;
use wayland_server::backend::ObjectId;

pub struct SceneState {
    pub space: Space<Window>,
    // Stable stacking order: bottom -> top
    pub windows: IndexMap<WindowId, Window>,
    next_wid: u32,
    // Surface mappings
    ///pub wl_surface: HashMap<ObjectId, ToplevelSurface>,
    pub surface_to_wid: HashMap<ObjectId, WindowId>,
    pub wid_to_surface: HashMap<WindowId, ObjectId>,

    // Output mapping
    pub window_output: HashMap<WindowId, OutputId>,

    // Visibility tracking
    pub mapped_windows: HashSet<WindowId>,
    pub mapped_surfaces: HashSet<ObjectId>,

    // Focus / MRU
    pub focused: Option<WindowId>,
    pub mru: Vec<WindowId>,
}

impl SceneState {
    pub fn new() -> Self {
        Self {
            space: Space::<Window>::default(),
            windows: IndexMap::new(),
            next_wid: 1,
            surface_to_wid: HashMap::new(),
            wid_to_surface: HashMap::new(),
            window_output: HashMap::new(),
            mapped_windows: HashSet::new(),
            mapped_surfaces: HashSet::new(),
            focused: None,
            mru: Vec::new(),
        }
    }
    pub fn next_mru_window(&mut self) -> Option<WindowId> {
        if self.mru.len() < 2 {
            return None;
        }

        self.mru.rotate_left(1);
        let id = self.mru.first().copied();

        //let id = self.mru.remove(1);
        //self.mru.insert(0, id);
        id
    }

    pub fn prev_mru_window(&mut self) -> Option<WindowId> {
        if self.mru.len() < 2 {
            return None;
        }

        self.mru.rotate_right(1);
        let id = self.mru.first().copied();
        //let id = self.mru.pop()?;
        //self.mru.insert(0, id);
        id
    }

    pub fn cycle_mru_window(&self, current: Option<WindowId>, direction: i32) -> Option<WindowId> {
        if self.mru.len() < 2 {
            return None;
        }

        let current_id = current.or(self.focused)?;

        let idx = self.mru.iter().position(|&id| id == current_id)?;
        let len = self.mru.len() as i32;

        let next_idx = (idx as i32 + direction).rem_euclid(len) as usize;
        self.mru.get(next_idx).copied()
    }

    pub fn close_focused(&mut self) -> Option<WindowId> {
        let wid = self.focused?;

        self.windows.shift_remove(&wid);
        self.window_output.remove(&wid);
        self.mapped_windows.remove(&wid);
        self.mru.retain(|id| *id != wid);

        if self.focused == Some(wid) {
            self.focused = self.mru.first().copied();
        }

        Some(wid)
    }

    pub fn focus_window(&mut self, id: WindowId) {
        let Some(window) = self.windows.get(&id).cloned() else {
            return;
        };
        // set focus
        self.focused = Some(id);

        // MRU update
        self.mru.retain(|&x| x != id);
        self.mru.insert(0, id);
        self.mru.truncate(MRU_CAP);
        // bring to front (common WM behavior)
        //self.raise_window(id);
        self.space.raise_element(&window, true);
    }

    pub fn raise_window(&mut self, id: WindowId) {
        if let Some((key, win)) = self.windows.shift_remove_entry(&id) {
            self.windows.insert(key, win); // end = top
        }
    }

    pub fn layout_snapshot(&self) -> LayoutSnapshot {
        // Option 1: if you already have a constructor/helper:
        // LayoutSnapshot::from_scene(self)

        // Option 2: if LayoutSnapshot implements Default (good for “get it compiling”):
        LayoutSnapshot::default()
    }

    #[inline]
    pub fn next_wid(&mut self) -> WindowId {
        let id = self.next_wid;
        self.next_wid = self.next_wid.wrapping_add(1);

        // optional safety: avoid returning 0 if wrapping ever occurs (super unlikely)
        if self.next_wid == 0 {
            self.next_wid = 1;
        }

        WindowId(id)
    }
}
