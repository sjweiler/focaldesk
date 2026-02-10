use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct OutputId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct TaskId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveContext {
    None,
    Pinned(u8), // 1..=9
    Overflow(TaskId),
}

#[derive(Debug, Clone)]
pub struct TaskMeta {
    pub title: String,
    pub app_id: String,
    pub last_focus_tick: u64,
    pub created_tick: u64,
}

impl TaskMeta {
    pub fn new(app_id: impl Into<String>, title: impl Into<String>, now: u64) -> Self {
        Self {
            title: title.into(),
            app_id: app_id.into(),
            last_focus_tick: now,
            created_tick: now,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputState {
    pub pinned: [Option<TaskId>; 9],
    pub overflow: Vec<TaskId>,
    pub active: ActiveContext,
    pub last_active_pinned: Option<u8>,
    pub mru: VecDeque<TaskId>,
}

impl OutputState {
    pub fn new() -> Self {
        Self {
            pinned: [None; 9],
            overflow: Vec::new(),
            active: ActiveContext::None,
            last_active_pinned: None,
            mru: VecDeque::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Indicators {
    pub net_route: super::events::NetRoute,
    pub vpn_active: bool,
    pub has_battery: bool,
    pub on_ac: bool,
    pub charging: bool,
    pub mic_active: bool,
    pub cam_active: bool,
    pub recording_active: bool,
    pub mode: Option<super::events::Mode>,
}

impl Default for Indicators {
    fn default() -> Self {
        Self {
            net_route: super::events::NetRoute::Offline,
            vpn_active: false,
            has_battery: false,
            on_ac: true,
            charging: false,
            mic_active: false,
            cam_active: false,
            recording_active: false,
            mode: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FlowState {
    pub tick: u64,
    pub focused_output: OutputId,
    pub outputs: HashMap<OutputId, OutputState>,
    pub task_home: HashMap<TaskId, OutputId>,
    pub task_meta: HashMap<TaskId, TaskMeta>,
    pub indicators: Indicators,
    pub search_scope: super::events::Scope,
}

impl FlowState {
    pub fn new(primary: OutputId) -> Self {
        let mut outputs = HashMap::new();
        outputs.insert(primary, OutputState::new());

        Self {
            tick: 0,
            focused_output: primary,
            outputs,
            task_home: HashMap::new(),
            task_meta: HashMap::new(),
            indicators: Indicators::default(),
            search_scope: super::events::Scope::Local(primary),
        }
    }

    pub fn output_mut(&mut self, out: OutputId) -> &mut OutputState {
        self.outputs.entry(out).or_insert_with(OutputState::new)
    }
}

