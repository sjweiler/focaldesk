//! The login flow as a pure state machine, decoupled from both the UI and
//! the socket so it can be unit-tested headlessly. The egui layer renders
//! `LoginState` and feeds `UiEvent`s in; the calloop IPC source feeds
//! daemon `Response`s in; both produce `Request`s to send out.

use zeroize::Zeroizing;

use crate::ipc_client::{AuthMessageStyle, Request, Response};

#[derive(Debug)]
pub enum LoginState {
    /// Username field focused. `error` carries the previous failure, if any.
    EnterUsername { username: String, error: Option<String> },
    /// A request is in flight; show a spinner, ignore typing.
    Waiting { username: String },
    /// PAM asked a question (usually the password prompt).
    Prompt {
        username: String,
        style: AuthMessageStyle,
        message: String,
        /// Wiped on submit/cancel/drop.
        input: Zeroizing<String>,
        /// Info/error lines PAM emitted before this prompt (e.g. an
        /// expired-password warning from pam_unix).
        notices: Vec<(AuthMessageStyle, String)>,
    },
    /// SessionStarted received — stop accepting input; the daemon will
    /// SIGTERM us momentarily. Exit the event loop cleanly.
    Done,
}

impl Default for LoginState {
    fn default() -> Self {
        LoginState::EnterUsername {
            username: String::new(),
            error: None,
        }
    }
}

/// What the UI reports each frame.
pub enum UiEvent {
    /// Enter pressed on the username field.
    SubmitUsername,
    /// Enter pressed on a PAM prompt.
    SubmitPrompt,
    /// Escape pressed anywhere.
    Cancel,
}

impl LoginState {
    /// Apply a UI event; returns requests to queue on the socket.
    pub fn on_ui_event(&mut self, ev: UiEvent) -> Vec<Request> {
        match (std::mem::take(self), ev) {
            (LoginState::EnterUsername { username, .. }, UiEvent::SubmitUsername)
                if !username.trim().is_empty() =>
            {
                let username = username.trim().to_owned();
                *self = LoginState::Waiting {
                    username: username.clone(),
                };
                vec![Request::CreateSession { username }]
            }

            (LoginState::Prompt { username, input, .. }, UiEvent::SubmitPrompt) => {
                *self = LoginState::Waiting { username };
                // String moves out of Zeroizing here and into the outbox,
                // which zeroizes on drain. No lingering copy in the state.
                vec![Request::PostAuthResponse {
                    response: Some(input.to_string()),
                }]
            }

            (LoginState::Prompt { username, .. }, UiEvent::Cancel)
            | (LoginState::Waiting { username }, UiEvent::Cancel) => {
                *self = LoginState::EnterUsername {
                    username,
                    error: None,
                };
                vec![Request::CancelSession]
            }

            // Everything else: no transition, restore state.
            (state, _) => {
                *self = state;
                vec![]
            }
        }
    }

    /// Apply a daemon response.
    pub fn on_response(&mut self, resp: Response) {
        match (std::mem::take(self), resp) {
            // A prompt arrives while waiting: show it.
            (
                LoginState::Waiting { username },
                Response::AuthMessage { style, message },
            ) => match style {
                AuthMessageStyle::Secret | AuthMessageStyle::Visible => {
                    *self = LoginState::Prompt {
                        username,
                        style,
                        message,
                        input: Zeroizing::new(String::new()),
                        notices: Vec::new(),
                    };
                }
                // Display-only message with no prompt yet: keep waiting,
                // but we have nowhere to pin it — stash as a prompt-less
                // notice by re-entering Waiting. (The UI shows the last
                // notice next to the spinner; see ui.rs.)
                AuthMessageStyle::Info | AuthMessageStyle::Error => {
                    tracing::info!(%message, "pam notice");
                    *self = LoginState::Waiting { username };
                }
            },

            // Additional info/error while a prompt is already up.
            (
                LoginState::Prompt {
                    username,
                    style,
                    message,
                    input,
                    mut notices,
                },
                Response::AuthMessage {
                    style: n_style @ (AuthMessageStyle::Info | AuthMessageStyle::Error),
                    message: n_msg,
                },
            ) => {
                notices.push((n_style, n_msg));
                *self = LoginState::Prompt {
                    username,
                    style,
                    message,
                    input,
                    notices,
                };
            }

            // A second *prompt* while one is up replaces it (PAM stacks
            // can chain prompts; the previous input is already submitted).
            (
                LoginState::Prompt { username, .. },
                Response::AuthMessage { style, message },
            ) => {
                *self = LoginState::Prompt {
                    username,
                    style,
                    message,
                    input: Zeroizing::new(String::new()),
                    notices: Vec::new(),
                };
            }

            (LoginState::Waiting { username }, Response::AuthError { message })
            | (LoginState::Prompt { username, .. }, Response::AuthError { message }) => {
                *self = LoginState::EnterUsername {
                    username,
                    error: Some(message),
                };
            }

            (_, Response::SessionStarted) => {
                *self = LoginState::Done;
            }

            // Late/unexpected messages (e.g. AuthError after we cancelled):
            // ignore, keep current state.
            (state, resp) => {
                tracing::debug!(?resp, "ignoring response in current state");
                *self = state;
            }
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self, LoginState::Done)
    }
}

// std::mem::take needs Default; Default is EnterUsername (empty).

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_user(state: &mut LoginState, name: &str) -> Vec<Request> {
        *state = LoginState::EnterUsername {
            username: name.into(),
            error: None,
        };
        state.on_ui_event(UiEvent::SubmitUsername)
    }

    #[test]
    fn happy_path() {
        let mut s = LoginState::default();

        let reqs = submit_user(&mut s, "steven");
        assert!(matches!(reqs.as_slice(), [Request::CreateSession { .. }]));
        assert!(matches!(s, LoginState::Waiting { .. }));

        s.on_response(Response::AuthMessage {
            style: AuthMessageStyle::Secret,
            message: "Password: ".into(),
        });
        let LoginState::Prompt { ref mut input, style, .. } = s else {
            panic!("expected prompt");
        };
        assert_eq!(style, AuthMessageStyle::Secret);
        input.push_str("hunter2");

        let reqs = s.on_ui_event(UiEvent::SubmitPrompt);
        assert!(matches!(
            reqs.as_slice(),
            [Request::PostAuthResponse { response: Some(_) }]
        ));

        s.on_response(Response::SessionStarted);
        assert!(s.is_done());
    }

    #[test]
    fn auth_failure_returns_to_username_with_error() {
        let mut s = LoginState::default();
        submit_user(&mut s, "steven");
        s.on_response(Response::AuthError {
            message: "authentication failed".into(),
        });
        assert!(matches!(
            s,
            LoginState::EnterUsername { error: Some(_), .. }
        ));
    }

    #[test]
    fn cancel_from_prompt_sends_cancel_session() {
        let mut s = LoginState::default();
        submit_user(&mut s, "steven");
        s.on_response(Response::AuthMessage {
            style: AuthMessageStyle::Secret,
            message: "Password: ".into(),
        });
        let reqs = s.on_ui_event(UiEvent::Cancel);
        assert!(matches!(reqs.as_slice(), [Request::CancelSession]));
        assert!(matches!(s, LoginState::EnterUsername { .. }));
    }

    #[test]
    fn empty_username_is_ignored() {
        let mut s = LoginState::default();
        let reqs = s.on_ui_event(UiEvent::SubmitUsername);
        assert!(reqs.is_empty());
        assert!(matches!(s, LoginState::EnterUsername { .. }));
    }

    #[test]
    fn info_notice_attaches_to_prompt() {
        let mut s = LoginState::default();
        submit_user(&mut s, "steven");
        s.on_response(Response::AuthMessage {
            style: AuthMessageStyle::Secret,
            message: "Password: ".into(),
        });
        s.on_response(Response::AuthMessage {
            style: AuthMessageStyle::Info,
            message: "Your password expires in 3 days".into(),
        });
        let LoginState::Prompt { notices, .. } = &s else {
            panic!("expected prompt");
        };
        assert_eq!(notices.len(), 1);
    }
}
