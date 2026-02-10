// flowos/compositor/src/keybinds.rs

use smithay::input::keyboard::{KeyState, Keysym};
use smithay::input::Seat;

use flowos_policy::state::OutputId;

use crate::actions::{execute_actions, ExecCtx};
use crate::CompositorState;

pub fn handle_key(
    state: &mut CompositorState,
    seat: &Seat<CompositorState>,
    keysym: Keysym,
    key_state: KeyState,
    mod_down: bool,
    shift_down: bool,
) {
    if key_state != KeyState::Pressed {
        return;
    }
    if !mod_down {
        return;
    }

    let out = OutputId(state.focused_output_id());

    let actions = match keysym {
        Keysym::KEY_1..=Keysym::KEY_9 => {
            let slot = (keysym.raw() - Keysym::KEY_1.raw() + 1) as u8;
            if shift_down {
                state.policy_adapter.ev_move_focused_to_pinned(out, slot)
            } else {
                state.policy_adapter.ev_focus_pinned(out, slot)
            }
        }
        Keysym::KEY_Tab => {
            // shift+tab reverse if you want
            state.policy_adapter.ev_focus_next_pinned(out, shift_down)
        }
        Keysym::KEY_space => {
            // For now: open palette UI (not in policy yet) OR set a UI flag.
            // Keep policy pure; UI visibility is a compositor concern.
            state.ui.search_palette_open = true;
            vec![]
        }
        _ => vec![],
    };

    if actions.is_empty() {
        return;
    }

    let ctx = state.exec_ctx(seat);
    execute_actions(&ctx, actions);
}
