mod drm_backend;
mod flow;
mod font;
mod greetd;
mod keymap;
mod state;

use std::sync::mpsc;
use std::time::Duration;

use anyhow::anyhow;
use focaldesk_logging::{flog, init_default_logging, startup_banner};
use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::session::Event as SessionEvent;
use smithay::reexports::calloop::{self, EventLoop};

use drm_backend::GreeterOutput;
use keymap::Modifiers;
use state::LoginScreenState;

struct GreeterLoopData {
    state: LoginScreenState,
    req_tx: mpsc::Sender<greetd::Request>,
    output: GreeterOutput,
    mods: Modifiers,
    session_active: bool,
}

impl GreeterLoopData {
    fn render(&mut self) {
        if let Err(err) = self.output.render(&self.state) {
            flog(format!("greeter render failed: {err:?}"));
        }
    }
}

fn main() -> anyhow::Result<()> {
    init_default_logging();
    startup_banner(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), "drm");

    let mut event_loop: EventLoop<GreeterLoopData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let (req_tx, req_rx) = mpsc::channel::<greetd::Request>();
    let (resp_tx, resp_rx) = calloop::channel::channel::<anyhow::Result<greetd::Response>>();
    let _greetd_thread = greetd::spawn(req_rx, resp_tx)?;

    let (output, session_notifier, libinput) = GreeterOutput::open()?;
    let libinput_backend = LibinputInputBackend::new(libinput);

    loop_handle
        .insert_source(resp_rx, |event, _, data| {
            if let calloop::channel::Event::Msg(result) = event {
                flow::on_response(&mut data.state, &data.req_tx, result);
                data.render();
            }
        })
        .map_err(|err| anyhow!("failed to register greetd response channel: {err}"))?;

    loop_handle
        .insert_source(session_notifier, |event, _, data| match event {
            SessionEvent::PauseSession => {
                flog("greeter session paused");
                data.session_active = false;
            }
            SessionEvent::ActivateSession => {
                flog("greeter session activated");
                data.session_active = true;
                if let Err(err) = data.output.reassert_scanout() {
                    flog(format!("failed to reassert scanout on resume: {err:?}"));
                }
                data.render();
            }
        })
        .map_err(|err| anyhow!("failed to register session notifier: {err}"))?;

    loop_handle
        .insert_source(libinput_backend, |event, _, data| {
            handle_input_event(data, &event);
        })
        .map_err(|err| anyhow!("failed to register libinput backend: {err}"))?;

    let mut data = GreeterLoopData {
        state: LoginScreenState::new(),
        req_tx,
        output,
        mods: Modifiers::default(),
        session_active: true,
    };
    data.render();

    loop {
        event_loop.dispatch(Some(Duration::from_millis(16)), &mut data)?;
    }
}

fn handle_input_event(data: &mut GreeterLoopData, event: &InputEvent<LibinputInputBackend>) {
    let InputEvent::Keyboard { event, .. } = event else {
        return;
    };

    let keycode: u32 = event.key_code().into();
    let pressed = event.state() == KeyState::Pressed;

    if data.mods.track(keycode, pressed) {
        return;
    }

    if !pressed {
        return;
    }

    if data.mods.ctrl && data.mods.alt {
        if let Some(vt) = keymap::vt_switch_target(keycode) {
            if let Err(err) = data.output.change_vt(vt) {
                flog(format!("{err:?}"));
            }
            return;
        }
    }

    match keycode {
        keymap::KEY_ENTER => {
            if let Err(err) = flow::submit(&mut data.state, &data.req_tx) {
                flog(format!("greetd request failed: {err:?}"));
            }
        }
        keymap::KEY_BACKSPACE => data.state.backspace(),
        keymap::KEY_ESC => flow::cancel(&mut data.state, &data.req_tx),
        code => {
            if let Some(ch) = keymap::keycode_to_char(code, data.mods.shift, data.mods.caps) {
                data.state.push_char(ch);
            }
        }
    }

    data.render();
}
