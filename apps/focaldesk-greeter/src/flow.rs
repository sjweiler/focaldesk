use std::sync::mpsc::Sender;

use crate::greetd::{self, AuthMessageType, ErrorType, Response};
use crate::state::{LoginPhase, LoginScreenState};

const SESSION_CMD: &str = "/usr/local/bin/focaldesk-desktop";

/// Called when the user hits Enter. Advances the state machine and, where
/// applicable, sends the next greetd request. `state.input` is the line the
/// user just finished typing (username or a response to the current prompt).
pub fn submit(state: &mut LoginScreenState, req_tx: &Sender<greetd::Request>) -> anyhow::Result<()> {
    match &state.phase {
        LoginPhase::EnteringUsername => {
            state.username = std::mem::take(&mut state.input).trim().to_string();
            let username = state.username.clone();
            state.phase = LoginPhase::Authenticating;
            req_tx.send(greetd::Request::CreateSession { username })?;
        }
        LoginPhase::EnteringResponse { .. } => {
            let answer = std::mem::take(&mut state.input);
            state.phase = LoginPhase::Authenticating;
            req_tx.send(greetd::Request::PostAuthMessageResponse {
                response: Some(answer),
            })?;
        }
        LoginPhase::Failed { .. } => {
            // Enter just acknowledges the failure; greetd's session is already
            // dead, so the next real attempt starts over from EnteringUsername.
            state.reset_for_retry();
        }
        LoginPhase::Authenticating | LoginPhase::Cancelling | LoginPhase::Starting => {
            // No input accepted while waiting on greetd.
        }
    }
    Ok(())
}

/// Called on Escape. What it does depends on how far into a login attempt we
/// are: nothing to tell greetd about yet (just clear the field), a live
/// session to tear down (`CancelSession`, then wait for the ack), or nothing
/// left to cancel (already dead, or already starting).
pub fn cancel(state: &mut LoginScreenState, req_tx: &Sender<greetd::Request>) {
    match &state.phase {
        LoginPhase::EnteringUsername => {
            state.clear_input();
        }
        LoginPhase::EnteringResponse { .. } | LoginPhase::Authenticating => {
            state.phase = LoginPhase::Cancelling;
            if req_tx.send(greetd::Request::CancelSession).is_err() {
                // IPC thread is gone; no ack will ever arrive, so don't get stuck
                // in Cancelling forever.
                state.reset_for_retry();
            }
        }
        LoginPhase::Failed { .. } => {
            // Session is already dead (greetd killed it on the auth error);
            // nothing to cancel, just acknowledge and start over.
            state.reset_for_retry();
        }
        LoginPhase::Cancelling | LoginPhase::Starting => {
            // Already cancelling, or too late — StartSession is in flight.
        }
    }
}

/// Called when a `Response` arrives from the greetd IPC thread.
pub fn on_response(
    state: &mut LoginScreenState,
    req_tx: &Sender<greetd::Request>,
    result: anyhow::Result<Response>,
) {
    // A pending CancelSession ends the login attempt no matter what greetd
    // says back — Success, Error, or a transport error all mean the same
    // thing here: the old session is gone, start fresh.
    if matches!(state.phase, LoginPhase::Cancelling) {
        state.reset_for_retry();
        return;
    }

    match result {
        Ok(Response::AuthMessage {
            auth_message_type,
            auth_message,
        }) => {
            state.phase = LoginPhase::EnteringResponse {
                secret: matches!(auth_message_type, AuthMessageType::Secret),
                prompt: auth_message,
            };
        }
        // greetd sends `Success` twice in a full login: once when auth is
        // complete (our cue to call StartSession) and again once the session
        // actually started (our cue to get out of the way). `state.phase`
        // tells the two apart — `Starting` means we already sent StartSession.
        Ok(Response::Success) if matches!(state.phase, LoginPhase::Starting) => {
            std::process::exit(0);
        }
        Ok(Response::Success) => {
            state.phase = LoginPhase::Starting;
            let start = greetd::Request::StartSession {
                cmd: vec![SESSION_CMD.to_string()],
                env: vec![
                    "XDG_SESSION_TYPE=wayland".to_string(),
                    "XDG_CURRENT_DESKTOP=FocalDesk".to_string(),
                ],
            };
            // Best-effort: if this send fails the IPC thread is already gone,
            // which the next response (or its absence) will surface anyway.
            let _ = req_tx.send(start);
        }
        Ok(Response::Error {
            error_type,
            description,
        }) => {
            let message = match error_type {
                ErrorType::AuthError => format!("authentication failed: {description}"),
                ErrorType::Error => format!("greetd error: {description}"),
            };
            state.phase = LoginPhase::Failed { message };
        }
        Err(err) => {
            state.phase = LoginPhase::Failed {
                message: format!("greetd IPC error: {err}"),
            };
        }
    }
}
