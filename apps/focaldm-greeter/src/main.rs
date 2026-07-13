//! focaldm-greeter entry point.
//!
//! NOT a Wayland compositor: no listening socket, no protocol globals, no
//! clients. Just DRM output + libinput + a socket to focaldmd.
//!
//! The greeter now behaves more like a lock screen: animated background,
//! centered panel, readable font, and a clickable power menu.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _};
use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::session::Event as SessionEvent;
use smithay::reexports::calloop::{
    generic::Generic, EventLoop, Interest, LoopSignal, Mode, PostAction,
};

use focaldm_greeter::drm_backend::GreeterOutput;
use focaldm_greeter::ipc_client::{DaemonConnection, Request};
use focaldm_greeter::keymap::{self, Modifiers};
use focaldm_greeter::login::{LoginState, UiEvent};
use focaldm_greeter::render::{self, FrameHitTargets, PowerAction};

struct Greeter {
    login: LoginState,
    conn: DaemonConnection,
    output: GreeterOutput,
    mods: Modifiers,
    session_active: bool,
    power_menu_open: bool,
    pointer: Option<(i32, i32)>,
    frame: FrameHitTargets,
    started_at: Instant,
    signal: LoopSignal,
}

impl Greeter {
    fn render(&mut self) {
        if !self.session_active {
            return;
        }

        let frame = render::FrameState {
            login: &self.login,
            pointer: self.pointer,
            power_menu_open: self.power_menu_open,
            pulse_phase: self.started_at.elapsed().as_secs_f32(),
            background_style: self.output.background_style(),
            // Ignored by `GreeterOutput::render`, which decides for itself
            // whether the CPU fallback path needs to paint its own
            // background this frame; kept only because `FrameState` is
            // also `paint_frame`'s standalone public contract.
            paint_background: false,
        };

        let started = Instant::now();
        match self.output.render(&frame) {
            Ok(layout) => self.frame = layout,
            Err(e) => tracing::error!(error = ?e, "render failed"),
        }
        tracing::debug!(
            elapsed_us = started.elapsed().as_micros(),
            "greeter frame rendered"
        );
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

    fn submit_ui_event(&mut self, ev: UiEvent) {
        let reqs = self.login.on_ui_event(ev);
        self.send_all(reqs);
    }

    fn close_power_menu(&mut self) {
        self.power_menu_open = false;
    }

    fn launch_power_action(action: PowerAction) {
        std::thread::spawn(move || {
            let mut command = Command::new("systemctl");
            command.arg("--no-block");
            match action {
                PowerAction::Suspend => {
                    command.arg("suspend");
                }
                PowerAction::Hibernate => {
                    command.arg("hibernate");
                }
                PowerAction::Restart => {
                    command.arg("reboot");
                }
                PowerAction::PowerOff => {
                    command.arg("poweroff");
                }
            }
            if let Err(e) = command.status() {
                tracing::error!(error = %e, action = ?action, "failed to launch power action");
            }
        });
    }

    fn handle_primary_click(&mut self) {
        let Some((px, py)) = self.pointer else {
            return;
        };

        if self.power_menu_open {
            for (action, rect) in &self.frame.power_menu_items {
                if rect.contains(px, py) {
                    Self::launch_power_action(*action);
                    self.close_power_menu();
                    return;
                }
            }
            if !self.frame.power_button.contains(px, py) {
                self.close_power_menu();
            }
            return;
        }

        if self.frame.power_button.contains(px, py) {
            self.power_menu_open = true;
        }
    }

    fn handle_pointer_motion(&mut self, dx: f64, dy: f64) {
        let (mut x, mut y) = self.pointer.unwrap_or((0, 0));
        x += dx.round() as i32;
        y += dy.round() as i32;
        self.pointer = Some((x, y));
    }

    fn handle_pointer_absolute(&mut self, x: i32, y: i32) {
        self.pointer = Some((x, y));
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
    let initially_active = output.is_session_active();

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
            |_, _, g: &mut Greeter| match g.conn.read_responses() {
                Ok(resps) => {
                    for r in resps {
                        g.login.on_response(r);
                    }
                    if g.login.is_done() {
                        // Daemon will SIGTERM us; leave the loop now so
                        // DRM master is released as fast as possible.
                        g.signal.stop();
                    }
                    g.render();
                    Ok(PostAction::Continue)
                }
                Err(e) => {
                    tracing::error!(error = %e, "daemon connection lost");
                    g.signal.stop();
                    Ok(PostAction::Remove)
                }
            },
        )
        .map_err(|e| anyhow!("insert socket source: {e}"))?;

    // Start the cursor at screen center rather than leaving it `None`
    // (invisible) until the first physical mouse-motion event arrives —
    // a lock screen should show a pointer immediately.
    let (mode_w, mode_h) = output.mode_size();
    let initial_pointer = Some((mode_w as i32 / 2, mode_h as i32 / 2));

    let mut greeter = Greeter {
        login: LoginState::default(),
        conn,
        output,
        mods: Modifiers::default(),
        session_active: initially_active,
        power_menu_open: false,
        pointer: initial_pointer,
        frame: FrameHitTargets::default(),
        started_at: Instant::now(),
        signal: event_loop.get_signal(),
    };

    let drm_fd = greeter.output.drm_fd();
    handle
        .insert_source(
            Generic::new(drm_fd, Interest::READ, Mode::Level),
            |_, _, g: &mut Greeter| match g.output.handle_drm_events() {
                Ok(_) => {
                    if g.session_active && !g.output.flip_pending() {
                        g.render();
                    }
                    Ok(PostAction::Continue)
                }
                Err(e) => {
                    tracing::error!(error = %e, "drm event handling failed");
                    g.signal.stop();
                    Ok(PostAction::Remove)
                }
            },
        )
        .map_err(|e| anyhow!("insert drm source: {e}"))?;

    if greeter.session_active {
        greeter.render();
    } else {
        tracing::warn!(
            "greeter started while seat inactive; waiting for ActivateSession before first frame"
        );
    }

    event_loop.run(Duration::from_millis(16), &mut greeter, |g| {
        if g.session_active && !g.output.flip_pending() {
            g.render();
        }
    })?;

    Ok(())
}

fn handle_input_event(g: &mut Greeter, event: &InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::Keyboard { event, .. } => {
            let keycode: u32 = event.key_code().into();
            let pressed = event.state() == KeyState::Pressed;

            if g.mods.track(keycode, pressed) {
                return;
            }
            if !pressed {
                return;
            }

            if g.power_menu_open && keycode == keymap::KEY_ESC {
                g.close_power_menu();
                g.render();
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
                        g.submit_ui_event(ev);
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

            if !g.output.flip_pending() {
                g.render();
            }
        }
        InputEvent::PointerMotion { event, .. } => {
            let delta = event.delta();
            g.handle_pointer_motion(delta.x, delta.y);
            if !g.output.flip_pending() {
                g.render();
            }
        }
        InputEvent::PointerMotionAbsolute { event, .. } => {
            let (w, h) = g.output.mode_size();
            let pos = event
                .position_transformed((w as i32, h as i32).into())
                .to_f64();
            g.handle_pointer_absolute(pos.x.round() as i32, pos.y.round() as i32);
            if !g.output.flip_pending() {
                g.render();
            }
        }
        InputEvent::PointerButton { event, .. } => {
            let button = event.button_code();
            let pressed = event.state() == ButtonState::Pressed;
            if button == 0x110 && pressed {
                g.handle_primary_click();
                if !g.output.flip_pending() {
                    g.render();
                }
            }
        }
        _ => {}
    }
}
