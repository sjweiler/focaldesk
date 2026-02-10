// flowos/policy/src/reducer.rs


use super::events::{FlowEvent, Intent, PrivacyDevice};
use super::state::{ActiveContext, FlowState, OutputId, OutputState, TaskId};


/// Actions are *intentional side effects* the Smithay adapter executes:
/// focusing a task, moving it between outputs, updating UI, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Request compositor focus to a given task (top-level).
    FocusTask(TaskId),

    /// Inform UI layers that a particular output’s nav highlight changed.
    /// (You can treat this as “schedule redraw” per output.)
    RedrawOutputChrome(OutputId),

    /// Inform UI layers that global top bar indicators changed.
    RedrawTopBar,

    /// Move a task to a different output (adapter performs actual surface move).
    MoveTaskToOutput { task: TaskId, to: OutputId },

    /// Close a task (optional; use only if policy ever triggers closes explicitly).
    CloseTask(TaskId),

    /// No-op marker (useful in debugging)
    Nop,
}

#[derive(Debug, Clone)]
pub struct ReduceResult {
    pub actions: Vec<Action>,
}

impl ReduceResult {
    pub fn new() -> Self {
        Self { actions: Vec::new() }
    }

    fn redraw_output(&mut self, out: OutputId) {
        self.actions.push(Action::RedrawOutputChrome(out));
    }
}

/// Reduce one event into state changes + actions.
/// Deterministic: no IO, no Smithay types, no time sources.
pub fn reduce(state: &mut FlowState, ev: FlowEvent) -> ReduceResult {
    state.tick = state.tick.saturating_add(1);
    let now = state.tick;

    let mut rr = ReduceResult::new();

    match ev {
        // ---------- Outputs ----------
        FlowEvent::OutputAdded { output, .. } => {
            state.outputs.entry(output).or_insert_with(OutputState::new);
            // If first time, you may keep focused_output as-is.
            rr.actions.push(Action::RedrawOutputChrome(output));
        }

        FlowEvent::OutputRemoved {
            output,
            intent: _,
            fallback_output,
        } => {
            // Migrate tasks to fallback output overflow
            if let Some(removed) = state.outputs.remove(&output) {
                // Move pinned tasks
                for slot_task in removed.pinned.iter().flatten() {
                    migrate_task(state, &mut rr, *slot_task, output, fallback_output, true);
                }
                // Move overflow tasks
                for t in removed.overflow.iter() {
                    migrate_task(state, &mut rr, *t, output, fallback_output, true);
                }
            }
            // Update focused output if needed
            if state.focused_output == output {
                state.focused_output = fallback_output;
            }
            rr.actions.push(Action::RedrawOutputChrome(fallback_output));
            rr.actions.push(Action::RedrawTopBar);
        }

        FlowEvent::FocusOutput { output, intent } => {
            if intent == Intent::User {
                state.focused_output = output;
                rr.redraw_output(output);
            }
        }

        // ---------- Tasks ----------
        FlowEvent::TaskCreated {
            task,
            output,
            intent,
            requested_slot,
        } => {
            // Record ownership
            state.task_home.insert(task, output);
            // Ensure output exists
            let out = state.output_mut(output);

            // Place task according to Auto-overflow policy
            if let Some(slot) = requested_slot {
                if is_valid_slot(slot) && out.pinned[(slot - 1) as usize].is_none() {
                    out.pinned[(slot - 1) as usize] = Some(task);
                    out.active = ActiveContext::Pinned(slot);
                    out.last_active_pinned = Some(slot);
                    mru_touch(out, task);
                    touch_meta(state, task, now);

                    // Focus only if user intent
                    if intent == Intent::User {
                        rr.actions.push(Action::FocusTask(task));
                    }
                    rr.redraw_output(output);
                } else {
                    // slot occupied -> overflow
                    out.overflow.push(task);
                    out.active = ActiveContext::Overflow(task);
                    mru_touch(out, task);
                    touch_meta(state, task, now);

                    if intent == Intent::User {
                        rr.actions.push(Action::FocusTask(task));
                    }
                    rr.redraw_output(output);
                }
            } else {
                // Default launch -> overflow
                out.overflow.push(task);
                out.active = ActiveContext::Overflow(task);
                mru_touch(out, task);
                touch_meta(state, task, now);

                if intent == Intent::User {
                    rr.actions.push(Action::FocusTask(task));
                }
                rr.redraw_output(output);
            }
        }

        FlowEvent::TaskClosed { task, intent: _ } => {
           if let Some(home) = state.task_home.remove(&task) {
           // --- scope 1: borrow OutputState only ---
           let was_active = {
             let out = state.output_mut(home);

             let was_active = matches_active(out, task);

              remove_task_from_output(out, task);
              out.mru.retain(|&x| x != task);

              if was_active {
                  out.active = ActiveContext::None;
              }

            was_active
          }; // <-- mutable borrow of `out` ends here

          // --- scope 2: now safe to borrow `state` again ---
          if was_active {
             choose_focus_after_close(state, &mut rr, home);
         }

         state.task_meta.remove(&task);
         rr.redraw_output(home);
        }
       }

        FlowEvent::TaskTitleUpdated { task, title } => {
            if let Some(meta) = state.task_meta.get_mut(&task) {
                meta.title = title;
            }
            // UI can choose to redraw if title is visible.
            // Keep it simple: redraw the home output chrome.
            if let Some(home) = state.task_home.get(&task).copied() {
                rr.redraw_output(home);
            }
        }

        // ---------- Focus & navigation ----------
        FlowEvent::FocusPinned { output, slot, intent } => {
            if intent != Intent::User {
                return rr; // no focus theft
            }
            if !is_valid_slot(slot) {
                return rr;
            }
            let out = state.output_mut(output);
            if let Some(task) = out.pinned[(slot - 1) as usize] {
                out.active = ActiveContext::Pinned(slot);
                out.last_active_pinned = Some(slot);
                mru_touch(out, task);
                touch_meta(state, task, now);
                rr.actions.push(Action::FocusTask(task));
                rr.redraw_output(output);
            }
        }

        FlowEvent::FocusOverflowTask { output, task, intent } => {
            if intent != Intent::User {
                return rr;
            }
            let out = state.output_mut(output);
            if out.overflow.contains(&task) {
                out.active = ActiveContext::Overflow(task);
                mru_touch(out, task);
                touch_meta(state, task, now);
                rr.actions.push(Action::FocusTask(task));
                rr.redraw_output(output);
            }
        }

        FlowEvent::FocusNextPinned {
            output,
            intent,
            reverse,
        } => {
            if intent != Intent::User {
                return rr;
            }
            let out = state.output_mut(output);
            if let Some(next_slot) = next_pinned_slot(out, reverse) {
                if let Some(task) = out.pinned[(next_slot - 1) as usize] {
                    out.active = ActiveContext::Pinned(next_slot);
                    out.last_active_pinned = Some(next_slot);
                    mru_touch(out, task);
                    touch_meta(state, task, now);
                    rr.actions.push(Action::FocusTask(task));
                    rr.redraw_output(output);
                }
            }
        }

        FlowEvent::MoveFocusedToPinned { output, slot, intent } => {
            if intent != Intent::User || !is_valid_slot(slot) {
                return rr;
            }
            let out = state.output_mut(output);
            let focused_task = match out.active {
                ActiveContext::Pinned(s) => out.pinned[(s - 1) as usize],
                ActiveContext::Overflow(t) => Some(t),
                ActiveContext::None => None,
            };
            let Some(t) = focused_task else { return rr; };

            move_task_into_slot(out, t, slot);
            out.active = ActiveContext::Pinned(slot);
            out.last_active_pinned = Some(slot);
            mru_touch(out, t);
            touch_meta(state, t, now);

            rr.actions.push(Action::FocusTask(t));
            rr.redraw_output(output);
        }

        FlowEvent::MoveTaskToOutput {
            task,
            from,
            to,
            intent,
            follow_focus,
        } => {
            // Move ownership regardless of intent; focus only if user.
            migrate_task(state, &mut rr, task, from, to, true);
            rr.actions.push(Action::MoveTaskToOutput { task, to });

            if follow_focus && intent == Intent::User {
                // Set active overflow on target
                let out_to = state.output_mut(to);
                out_to.active = ActiveContext::Overflow(task);
                mru_touch(out_to, task);
                touch_meta(state, task, now);
                rr.actions.push(Action::FocusTask(task));
            }

            rr.redraw_output(from);
            rr.redraw_output(to);
        }

        // ---------- Indicators ----------
        FlowEvent::NetworkRouteChanged { route, .. } => {
            state.indicators.net_route = route;
            rr.actions.push(Action::RedrawTopBar);
        }
        FlowEvent::VpnChanged { active, .. } => {
            state.indicators.vpn_active = active;
            rr.actions.push(Action::RedrawTopBar);
        }
        FlowEvent::PowerChanged {
            has_battery,
            on_ac,
            charging,
            ..
        } => {
            state.indicators.has_battery = has_battery;
            state.indicators.on_ac = on_ac;
            state.indicators.charging = charging;
            rr.actions.push(Action::RedrawTopBar);
        }
        FlowEvent::PrivacyChanged {
            device,
            active,
            ..
        } => {
            match device {
                PrivacyDevice::Mic => state.indicators.mic_active = active,
                PrivacyDevice::Camera => state.indicators.cam_active = active,
                PrivacyDevice::Recording => state.indicators.recording_active = active,
            }
            rr.actions.push(Action::RedrawTopBar);
        }
        FlowEvent::ModeChanged { mode, .. } => {
            state.indicators.mode = mode;
            rr.actions.push(Action::RedrawTopBar);
        }
        FlowEvent::SearchScopeChanged { scope, intent } => {
            if intent == Intent::User {
                state.search_scope = scope;
            }
        }
    }

    rr
}

// ---------------- helpers ----------------

fn is_valid_slot(slot: u8) -> bool {
    (1..=9).contains(&slot)
}

fn mru_touch(out: &mut OutputState, task: TaskId) {
    out.mru.retain(|&x| x != task);
    out.mru.push_front(task);
}

fn touch_meta(state: &mut FlowState, task: TaskId, now: u64) {
    state
        .task_meta
        .entry(task)
        .and_modify(|m| m.last_focus_tick = now)
        .or_insert_with(|| {
            // app_id/title may be filled later by adapter; use placeholders.
            super::state::TaskMeta::new("unknown", "untitled", now)
        });
}

fn remove_task_from_output(out: &mut OutputState, task: TaskId) {
    // Remove from pinned if present
    for s in 0..9 {
        if out.pinned[s] == Some(task) {
            out.pinned[s] = None;
        }
    }
    // Remove from overflow
    out.overflow.retain(|&t| t != task);
}

fn matches_active(out: &OutputState, task: TaskId) -> bool {
    match out.active {
        ActiveContext::Overflow(t) => t == task,
        ActiveContext::Pinned(slot) => {
            if !(1..=9).contains(&slot) {
                false
            } else {
                out.pinned[(slot - 1) as usize] == Some(task)
            }
        }
        ActiveContext::None => false,
    }
}

fn next_pinned_slot(out: &OutputState, reverse: bool) -> Option<u8> {
    // Numeric order cycling only, per policy.
    let occupied: Vec<u8> = (1u8..=9u8)
        .filter(|&s| out.pinned[(s - 1) as usize].is_some())
        .collect();
    if occupied.is_empty() {
        return None;
    }

    let current = match out.active {
        ActiveContext::Pinned(s) if (1..=9).contains(&s) => Some(s),
        ActiveContext::Overflow(_) => out.last_active_pinned,
        ActiveContext::None => out.last_active_pinned,
        _ => None,
    };

    if !reverse {
        if let Some(cur) = current {
            for &s in &occupied {
                if s > cur {
                    return Some(s);
                }
            }
        }
        Some(occupied[0])
    } else {
        if let Some(cur) = current {
            for &s in occupied.iter().rev() {
                if s < cur {
                    return Some(s);
                }
            }
        }
        Some(*occupied.last().unwrap())
    }
}

fn choose_focus_after_close(state: &mut FlowState, rr: &mut ReduceResult, output: OutputId) {
    let out = state.output_mut(output);

    // Rule: if overflow still has tasks -> focus MRU-first overflow
    if let Some(t) = out.mru.iter().copied().find(|t| out.overflow.contains(t)) {
        out.active = ActiveContext::Overflow(t);
        rr.actions.push(Action::FocusTask(t));
        return;
    }
    // Else pinned tasks remain -> focus last active pinned if occupied else first occupied
    if let Some(slot) = out.last_active_pinned {
        if is_valid_slot(slot) && out.pinned[(slot - 1) as usize].is_some() {
            let t = out.pinned[(slot - 1) as usize].unwrap();
            out.active = ActiveContext::Pinned(slot);
            rr.actions.push(Action::FocusTask(t));
            return;
        }
    }
    for slot in 1u8..=9u8 {
        if let Some(t) = out.pinned[(slot - 1) as usize] {
            out.active = ActiveContext::Pinned(slot);
            out.last_active_pinned = Some(slot);
            rr.actions.push(Action::FocusTask(t));
            return;
        }
    }
    out.active = ActiveContext::None;
}

fn migrate_task(
    state: &mut FlowState,
    rr: &mut ReduceResult,
    task: TaskId,
    from: OutputId,
    to: OutputId,
    insert_to_overflow: bool,
) {
    // Remove from `from`
    if let Some(out_from) = state.outputs.get_mut(&from) {
        remove_task_from_output(out_from, task);
        out_from.mru.retain(|&x| x != task);
        if matches_active(out_from, task) {
            choose_focus_after_close(state, rr, from);
        }
    }

    // Add to `to` overflow by policy
    let out_to = state.output_mut(to);
    if insert_to_overflow && !out_to.overflow.contains(&task) {
        out_to.overflow.push(task);
    }
    mru_touch(out_to, task);
    state.task_home.insert(task, to);
}

fn move_task_into_slot(out: &mut OutputState, task: TaskId, slot: u8) {
    let idx = (slot - 1) as usize;

    // Remove task from wherever it currently is in this output
    remove_task_from_output(out, task);

    // Slot handling
    match out.pinned[idx] {
        None => {
            out.pinned[idx] = Some(task);
        }
        Some(occupant) => {
            // Occupied: occupant goes to overflow; task becomes pinned
            out.pinned[idx] = Some(task);
            if !out.overflow.contains(&occupant) {
                out.overflow.insert(0, occupant);
            }
        }
    }
}
