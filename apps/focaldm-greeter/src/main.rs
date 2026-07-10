//! focaldm-greeter entry point.
//!
//! NOT a Wayland compositor: no listening socket, no protocol globals, no
//! clients. Just DRM output (dumb-buffer scanout, see `drm_backend`) +
//! libinput + the bitmap-font login box in `render`, plus the daemon socket.
//!
//! Event loop (calloop, same idiom as focaldesk-greeter and focaldesk
//! itself):
//!   - libseat session source   (VT/device management via smithay)
//!   - libinput source          -> keycodes -> LoginState mutation/events
//!   - daemon socket source     -> LoginState::on_response
//!
//! Render policy: damage-driven. Repaint on input and on IPC traffic. Idle
//! greeter = zero GPU/CPU work beyond waiting on fds.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::session::Event as SessionEvent;
use smithay::reexports::calloop::{
    generic::Generic, EventLoop, Interest, LoopSignal, Mode, PostAction,
};

use focaldm_greeter::drm_backend::GreeterOutput;
use focaldm_greeter::ipc_client::{DaemonConnection, Request};
use focaldm_greeter::keymap::{self, Modifiers};
use focaldm_greeter::login::{LoginState, UiEvent};

struct Greeter {
    login: LoginState,
    conn: DaemonConnection,
    output: GreeterOutput,
    mods: Modifiers,
    session_active: bool,
    needs_redraw: bool,
    signal: LoopSignal,
}

impl Greeter {
    fn render(&mut self) {
        if !self.session_active {
            return;
        }
        if let Err(e) = self.output.render(&self.login) {
            tracing::error!(error = ?e, "render failed");
        }
    }

    fn send_all(&mut self, reqs: Vec<Request>) {
        for req in reqs {
            if let Err(e) = self.conn.send(&req) {
                tracing::error!(error = %e, "failed to queue request");
            }
        }
        if let Err(e) = self.conn.flush() {
            tracing::error!(error = %e, "flush failed");
            self.signal.stop();
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let socket: PathBuf = std::env::var_os("FOCALDM_SOCKET")
        .context("FOCALDM_SOCKET not set — greeter must be launched by focaldmd")?
        .into();

    let mut event_loop: EventLoop<Greeter> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    let (output, session_notifier, libinput) = GreeterOutput::open()?;
    let libinput_backend = LibinputInputBackend::new(libinput);

    handle
        .insert_source(session_notifier, |event, _, g: &mut Greeter| match event {
            SessionEvent::PauseSession => {
                tracing::info!("greeter session paused");
                g.session_active = false;
            }
            SessionEvent::ActivateSession => {
                tracing::info!("greeter session activated");
                g.session_active = true;
                if let Err(e) = g.output.reassert_scanout() {
                    tracing::error!(error = ?e, "failed to reassert scanout on resume");
                }
                g.render();
            }
        })
        .map_err(|e| anyhow!("insert session notifier: {e}"))?;

    handle
        .insert_source(libinput_backend, |event, _, g: &mut Greeter| {
            handle_input_event(g, &event);
        })
        .map_err(|e| anyhow!("insert libinput backend: {e}"))?;

    let conn = DaemonConnection::connect(&socket)?;
    let sock_fd = conn.stream().try_clone()?;
    handle
        .insert_source(
            Generic::new(sock_fd, Interest::READ, Mode::Level),
            |_, _, g: &mut Greeter| {
                match g.conn.read_responses() {
                    Ok(resps) => {
                        for r in resps {
                            g.login.on_response(r);
                        }
                        g.render();
                        if g.login.is_done() {
                            // Daemon will SIGTERM us; leave the loop now so
                            // DRM master is released as fast as possible.
                            g.signal.stop();
                        }
                        Ok(PostAction::Continue)
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "daemon connection lost");
                        g.signal.stop();
                        Ok(PostAction::Remove)
                    }
                }
            },
        )
        .map_err(|e| anyhow!("insert socket source: {e}"))?;

    let mut greeter = Greeter {
        login: LoginState::default(),
        conn,
        output,
        mods: Modifiers::default(),
        session_active: true,
        needs_redraw: false,
        signal: event_loop.get_signal(),
    };
    greeter.render();

    event_loop.run(Duration::from_millis(16), &mut greeter, |g| {
        if !g.needs_redraw {
            return;
        }
        g.needs_redraw = false;
        g.render();
    })?;

    Ok(())
}

fn handle_input_event(g: &mut Greeter, event: &InputEvent<LibinputInputBackend>) {
    let InputEvent::Keyboard { event, .. } = event else {
        return;
    };

    let keycode: u32 = event.key_code().into();
    let pressed = event.state() == KeyState::Pressed;

    if g.mods.track(keycode, pressed) {
        return;
    }
    if !pressed {
        return;
    }

    if g.mods.ctrl && g.mods.alt {
        if let Some(vt) = keymap::vt_switch_target(keycode) {
            if let Err(e) = g.output.change_vt(vt) {
                tracing::error!(error = ?e, "VT switch failed");
            }
            return;
        }
    }

    match keycode {
        keymap::KEY_ENTER => {
            let ev = match &g.login {
                LoginState::EnterUsername { .. } => Some(UiEvent::SubmitUsername),
                LoginState::Prompt { .. } => Some(UiEvent::SubmitPrompt),
                LoginState::Waiting { .. } | LoginState::Done => None,
            };
            if let Some(ev) = ev {
                let reqs = g.login.on_ui_event(ev);
                g.send_all(reqs);
            }
        }
        keymap::KEY_BACKSPACE => match &mut g.login {
            LoginState::EnterUsername { username, .. } => {
                username.pop();
            }
            LoginState::Prompt { input, .. } => {
                input.pop();
            }
            LoginState::Waiting { .. } | LoginState::Done => {}
        },
        keymap::KEY_ESC => {
            let reqs = g.login.on_ui_event(UiEvent::Cancel);
            g.send_all(reqs);
        }
        code => {
            if let Some(ch) = keymap::keycode_to_char(code, g.mods.shift, g.mods.caps) {
                match &mut g.login {
                    LoginState::EnterUsername { username, .. } => username.push(ch),
                    LoginState::Prompt { input, .. } => input.push(ch),
                    LoginState::Waiting { .. } | LoginState::Done => {}
                }
            }
        }
    }

    g.render();
}
