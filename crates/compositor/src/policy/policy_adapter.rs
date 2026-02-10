// flowos/compositor/src/policy_adapter.rs

use std::collections::HashMap;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::xdg::XdgToplevelSurface;

use flowos_policy::events::{FlowEvent, Intent};
use flowos_policy::reducer::{reduce, Action};
use flowos_policy::state::{FlowState, OutputId, TaskId};

#[derive(Default)]
pub struct IdGen {
    next_task: u64,
}
impl IdGen {
    pub fn next_task_id(&mut self) -> TaskId {
        self.next_task = self.next_task.saturating_add(1);
        TaskId(self.next_task)
    }
}

/// What you need to focus a task in Smithay.
/// Store the toplevel handle + a stable surface reference.
#[derive(Clone)]
pub struct TaskHandle {
    pub toplevel: XdgToplevelSurface,
    pub surface: WlSurface,
}

pub struct PolicyAdapter {
    pub policy: FlowState,

    idgen: IdGen,

    // Smithay toplevel object id -> TaskId
    toplevel_to_task: HashMap<u32, TaskId>,
    // TaskId -> smithay handles needed to focus / move / render
    task_to_handle: HashMap<TaskId, TaskHandle>,
}

impl PolicyAdapter {
    pub fn new(primary_output: OutputId) -> Self {
        Self {
            policy: FlowState::new(primary_output),
            idgen: IdGen::default(),
            toplevel_to_task: HashMap::new(),
            task_to_handle: HashMap::new(),
        }
    }

    /// Utility: get a stable key for an XdgToplevelSurface
    fn key_for_toplevel(tl: &XdgToplevelSurface) -> u32 {
        // Smithay provides an "id" via the underlying wl_surface object.
        // If your build differs, replace this with your own stable key.
        tl.wl_surface().id().protocol_id()
    }

    pub fn on_xdg_toplevel_new(
        &mut self,
        tl: XdgToplevelSurface,
        output: OutputId,
        app_id: Option<String>,
        title: Option<String>,
    ) -> Vec<Action> {
        let key = Self::key_for_toplevel(&tl);

        let task = self.idgen.next_task_id();
        self.toplevel_to_task.insert(key, task);

        let surface = tl.wl_surface().clone();
        self.task_to_handle.insert(task, TaskHandle { toplevel: tl, surface });

        // Emit policy event
        let ev = FlowEvent::TaskCreated {
            task,
            output,
            intent: Intent::User,      // treat spawn as user intent if launched via user action
            requested_slot: None,      // default is Auto-overflow
        };

        let mut rr = reduce(&mut self.policy, ev);

        // Populate meta when you learn app_id/title (optional)
        if let Some(app_id) = app_id {
            if let Some(meta) = self.policy.task_meta.get_mut(&task) {
                meta.app_id = app_id;
            }
        }
        if let Some(title) = title {
            if let Some(meta) = self.policy.task_meta.get_mut(&task) {
                meta.title = title;
            }
        }

        rr.actions
    }

    pub fn on_xdg_toplevel_title_changed(&mut self, tl: &XdgToplevelSurface, title: String) -> Vec<Action> {
        let key = Self::key_for_toplevel(tl);
        let Some(task) = self.toplevel_to_task.get(&key).copied() else { return vec![]; };

        let rr = reduce(&mut self.policy, FlowEvent::TaskTitleUpdated { task, title });
        rr.actions
    }

    pub fn on_xdg_toplevel_app_id_changed(&mut self, tl: &XdgToplevelSurface, app_id: String) -> Vec<Action> {
        let key = Self::key_for_toplevel(tl);
        let Some(task) = self.toplevel_to_task.get(&key).copied() else { return vec![]; };

        // You didn’t define an explicit app_id event; simplest is to update meta directly.
        if let Some(meta) = self.policy.task_meta.get_mut(&task) {
            meta.app_id = app_id;
        }
        vec![Action::RedrawOutputChrome(self.policy.focused_output)]
    }

    pub fn on_xdg_toplevel_destroy(&mut self, tl: &XdgToplevelSurface) -> Vec<Action> {
        let key = Self::key_for_toplevel(tl);
        let Some(task) = self.toplevel_to_task.remove(&key) else { return vec![]; };

        self.task_to_handle.remove(&task);

        let rr = reduce(&mut self.policy, FlowEvent::TaskClosed { task, intent: Intent::System });
        rr.actions
    }

    /// Called by your keybinding handler: Mod+1..9
    pub fn ev_focus_pinned(&mut self, output: OutputId, slot: u8) -> Vec<Action> {
        reduce(&mut self.policy, FlowEvent::FocusPinned { output, slot, intent: Intent::User }).actions
    }

    /// Mod+Shift+1..9
    pub fn ev_move_focused_to_pinned(&mut self, output: OutputId, slot: u8) -> Vec<Action> {
        reduce(&mut self.policy, FlowEvent::MoveFocusedToPinned { output, slot, intent: Intent::User }).actions
    }

    /// Mod+Tab
    pub fn ev_focus_next_pinned(&mut self, output: OutputId, reverse: bool) -> Vec<Action> {
        reduce(&mut self.policy, FlowEvent::FocusNextPinned { output, intent: Intent::User, reverse }).actions
    }

    pub fn handle_for_task(&self, task: TaskId) -> Option<&TaskHandle> {
        self.task_to_handle.get(&task)
    }
}
