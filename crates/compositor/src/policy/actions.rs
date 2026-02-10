// flowos/compositor/src/actions.rs

use flowos_policy::reducer::Action;
use flowos_policy::state::TaskId;

use smithay::input::Seat;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::seat::WaylandFocus;

use crate::policy_adapter::PolicyAdapter;

/// Compositor-global execution context you likely already have.
pub struct ExecCtx<'a> {
    pub dh: &'a DisplayHandle,
    pub seat: &'a Seat<crate::CompositorState>,
    pub policy: &'a PolicyAdapter,

    // Hook to your renderer scheduling
    pub schedule_redraw_output: &'a dyn Fn(u32),
    pub schedule_redraw_topbar: &'a dyn Fn(),
}

pub fn execute_actions(ctx: &ExecCtx<'_>, actions: Vec<Action>) {
    for act in actions {
        match act {
            Action::FocusTask(task) => focus_task(ctx, task),
            Action::RedrawOutputChrome(out) => (ctx.schedule_redraw_output)(out.0),
            Action::RedrawTopBar => (ctx.schedule_redraw_topbar)(),
            Action::MoveTaskToOutput { task, to } => {
                // Implement when you have layout/output assignment in place.
                // For now, schedule redraw on both.
                (ctx.schedule_redraw_output)(to.0);
            }
            Action::CloseTask(_task) => {
                // Optional; your policy currently never requests closes automatically.
            }
            Action::Nop => {}
        }
    }
}

fn focus_task(ctx: &ExecCtx<'_>, task: TaskId) {
    let Some(handle) = ctx.policy.handle_for_task(task) else { return; };

    // This is the common Smithay pattern: set keyboard focus to the wl_surface.
    // Depending on your base, you may use `seat.get_keyboard().unwrap().set_focus(...)`.
    if let Some(kbd) = ctx.seat.get_keyboard() {
        kbd.set_focus(ctx.dh, Some(handle.surface.clone()), 0);
    }

    // Pointer focus typically follows keyboard focus in tiling/compositor models,
    // but you can keep it separate if desired.
}
