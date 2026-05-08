use std::collections::{HashMap, HashSet};

use flowstate_types::{OutputId, WindowId, WorkspaceId};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub windows: Vec<WindowId>,
    pub focused_window: Option<WindowId>,
}

#[derive(Debug, Default)]
pub struct WorkspaceStore {
    next_id: u32,
    workspaces: HashMap<WorkspaceId, Workspace>,
    order: Vec<WorkspaceId>,

    // which workspace is currently shown on an output
    current_by_output: HashMap<OutputId, WorkspaceId>,
}

impl WorkspaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_workspace(&mut self, name: impl Into<String>) -> WorkspaceId {
        let id = WorkspaceId(self.next_id);
        self.next_id += 1;

        let ws = Workspace {
            id,
            name: name.into(),
            windows: Vec::new(),
            focused_window: None,
        };

        self.workspaces.insert(id, ws);
        self.order.push(id);
        id
    }

    pub fn get(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.get(&id)
    }

    pub fn get_mut(&mut self, id: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces.get_mut(&id)
    }

    pub fn assign_output(&mut self, output_id: OutputId, workspace_id: WorkspaceId) {
        if self.workspaces.contains_key(&workspace_id) {
            self.current_by_output.insert(output_id, workspace_id);
        }
    }

    pub fn current_for_output(&self, output_id: OutputId) -> Option<WorkspaceId> {
        self.current_by_output.get(&output_id).copied()
    }

    pub fn add_window(&mut self, workspace_id: WorkspaceId, window_id: WindowId) {
        if let Some(ws) = self.workspaces.get_mut(&workspace_id) {
            if !ws.windows.contains(&window_id) {
                ws.windows.push(window_id);
            }
            ws.focused_window = Some(window_id);
        }
    }

    pub fn remove_window(&mut self, window_id: WindowId) {
        for ws in self.workspaces.values_mut() {
            ws.windows.retain(|wid| *wid != window_id);
            if ws.focused_window == Some(window_id) {
                ws.focused_window = ws.windows.last().copied();
            }
        }
    }

    pub fn move_window(&mut self, window_id: WindowId, target_workspace: WorkspaceId) {
        self.remove_window(window_id);
        self.add_window(target_workspace, window_id);
    }

    pub fn visible_windows_on_output(&self, output_id: OutputId) -> &[WindowId] {
        static EMPTY: [WindowId; 0] = [];
        if let Some(ws_id) = self.current_for_output(output_id) {
            if let Some(ws) = self.workspaces.get(&ws_id) {
                return &ws.windows;
            }
        }
        &EMPTY
    }
}
