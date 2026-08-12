#![allow(unused_imports)]

//! Shared setup for nested compositor backends (winit, future DRM/KMS, etc.).

use std::io;
use std::path::PathBuf;
#[cfg(feature = "xwayland")]
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
#[cfg(feature = "xwayland")]
use std::time::Duration;
use std::time::Instant;

use focaldesk_cursor::CursorManager;
use focaldesk_flow::Keybinds;
use focaldesk_logging::{flog, flog_info, session_id};
use focaldesk_notifications::NotificationSnapshot;
use focaldesk_resources::RenderResources;
use focaldesk_types::OutputId;
use focaldesk_ui::chrome::{Chrome, ChromeMetrics};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent,
    PointerMotionEvent,
};
use smithay::backend::renderer::gles::GlesTexture;
use smithay::input::keyboard::XkbConfig;
use smithay::output::{Output, PhysicalProperties, Subpixel};
#[cfg(feature = "xwayland")]
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle, ListeningSocket};
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use zbus::blocking::{Connection, MessageIterator};
use zbus::{MatchRule, MessageType};

use crate::core::desktop::{DesktopInit, DesktopState};
use crate::core::input::{
    FlowInputEvent, FlowKeyState, FlowModifiers, FlowMouseButton, FlowScrollDelta, FlowScrollSource,
};
use crate::core::render::RenderState;
use crate::core::ui_state::UiState;
use crate::core::wayland::client::ClientState;
use crate::core::OutputState;
use crate::core::SceneState;
use focaldesk_config::FocalDeskConfig;
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_settings_core::load_settings;
use focaldesk_themes::theme::BuiltInThemeId;
use focaldesk_themes::FlowThemeId;
use focaldesk_themes::ThemeManager;
use smithay::wayland::xdg_activation::XdgActivationState;
#[cfg(feature = "xwayland")]
use smithay::wayland::xwayland_shell::XWaylandShellState;
#[cfg(feature = "xwayland")]
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};
use tracing::{error, info, warn};

pub(crate) fn client_state_from_stream(stream: &std::os::unix::net::UnixStream) -> ClientState {
    ClientState::from_stream(stream)
}

pub(crate) fn physical_size_mm_from_pixels(size: Size<i32, Physical>) -> (i32, i32) {
    const MM_PER_INCH: f64 = 25.4;
    const FALLBACK_DPI: f64 = 96.0;

    let width = ((size.w.max(1) as f64) * MM_PER_INCH / FALLBACK_DPI).round() as i32;
    let height = ((size.h.max(1) as f64) * MM_PER_INCH / FALLBACK_DPI).round() as i32;
    (width.max(1), height.max(1))
}

pub(crate) struct BootstrapOutput {
    pub name: String,
    pub buffer_size: Size<i32, Physical>,
    pub scale_factor: f64,
}

pub(crate) fn is_nonfatal_wayland_io_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSleepEvent {
    GoingToSleep,
    WokeUp,
}

pub(crate) fn spawn_session_sleep_watch() -> io::Result<Receiver<SessionSleepEvent>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("focaldesk-session-sleep".into())
        .spawn(move || session_resume_watch_main(tx))?;
    Ok(rx)
}

pub(crate) fn drain_session_sleep_notifications(
    rx: &Receiver<SessionSleepEvent>,
    state: &mut DesktopState,
) {
    while let Ok(event) = rx.try_recv() {
        match event {
            SessionSleepEvent::GoingToSleep => state.handle_session_suspend(),
            SessionSleepEvent::WokeUp => state.handle_session_resume(),
        }
    }
}

fn session_resume_watch_main(notify: Sender<SessionSleepEvent>) {
    let Ok(conn) = Connection::system() else {
        flog("session resume watch: no system D-Bus");
        return;
    };

    let rule = match MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.login1")
        .and_then(|builder| builder.interface("org.freedesktop.login1.Manager"))
        .and_then(|builder| builder.member("PrepareForSleep"))
        .and_then(|builder| builder.path("/org/freedesktop/login1"))
    {
        Ok(builder) => builder.build(),
        Err(err) => {
            flog(format!(
                "session resume watch: failed to build match rule: {err}"
            ));
            return;
        }
    };

    let Ok(mut iter) = MessageIterator::for_match_rule(rule, &conn, Some(8)) else {
        flog("session resume watch: failed to subscribe to login1 PrepareForSleep");
        return;
    };

    flog("session sleep watch: listening for login1 PrepareForSleep");

    let mut last_sleeping: Option<bool> = None;
    loop {
        let Some(Ok(msg)) = iter.next() else {
            continue;
        };

        let Ok((sleeping,)): Result<(bool,), _> = msg.body() else {
            continue;
        };

        if last_sleeping == Some(sleeping) {
            continue;
        }
        last_sleeping = Some(sleeping);

        let event = if sleeping {
            SessionSleepEvent::GoingToSleep
        } else {
            SessionSleepEvent::WokeUp
        };
        let _ = notify.send(event);
    }
}

/// Spawn XWayland and register it on `handle`. Sets `DISPLAY` once the server is up.
#[cfg(feature = "xwayland")]
pub fn start_xwayland(
    state: &mut DesktopState,
    display_handle: &DisplayHandle,
    handle: LoopHandle<'static, DesktopState>,
) -> anyhow::Result<()> {
    info!(
        target: "focaldesk",
        session_id = session_id(),
        backend = "xwayland",
        "preparing to launch XWayland server"
    );
    state.xwayland_loop_handle = Some(handle.clone());
    let xwayland_env = std::env::vars().filter(|(key, _)| {
        matches!(
            key.as_str(),
            "DRI_PRIME"
                | "GBM_BACKEND"
                | "LIBGL_DEBUG"
                | "LIBGL_DRIVERS_PATH"
                | "MESA_LOADER_DRIVER_OVERRIDE"
                | "VK_ICD_FILENAMES"
                | "__GLX_VENDOR_LIBRARY_NAME"
                | "__NV_PRIME_RENDER_OFFLOAD"
                | "__VK_LAYER_NV_optimus"
        )
    });
    let (xwayland, xwayland_client) = XWayland::spawn(
        display_handle,
        None,
        xwayland_env,
        std::iter::empty::<&str>(),
        true,
        Stdio::inherit(),
        Stdio::inherit(),
        |_| (),
    )?;
    state.xwayland_client = Some(xwayland_client.clone());
    let xwayland_display = format!(":{}", xwayland.display_number());
    state.xwayland_display = Some(xwayland_display.clone());
    std::env::set_var("DISPLAY", &xwayland_display);
    info!(
        target: "focaldesk",
        session_id = session_id(),
        display = %xwayland_display,
        "launched XWayland and reserved DISPLAY"
    );

    let wm_handle = handle.clone();
    let wm_dh = display_handle.clone();
    handle.insert_source(xwayland, move |event, _, data| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            let xwayland_display_name = format!(":{display_number}");
            data.xwayland_display = Some(xwayland_display_name.clone());
            std::env::set_var("DISPLAY", &xwayland_display_name);
            info!(
                target: "focaldesk",
                session_id = session_id(),
                xwayland_display = %xwayland_display_name,
                socket_fd = ?x11_socket,
                "XWayland ready"
            );

            match X11Wm::start_wm(
                wm_handle.clone(),
                &wm_dh,
                x11_socket,
                xwayland_client.clone(),
            ) {
                Ok(wm) => {
                    data.xwm = Some(wm);
                    info!(
                        target: "focaldesk",
                        session_id = session_id(),
                        xwayland_display = %xwayland_display_name,
                        "XWayland WM attached"
                    );
                }
                Err(err) => {
                    error!(
                        target: "focaldesk",
                        session_id = session_id(),
                        xwayland_display = %xwayland_display_name,
                        error = %err,
                        "failed to start XWayland WM"
                    );
                    data.disable_xwayland();
                }
            }
        }
        XWaylandEvent::Error => {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                "XWayland failed to start"
            );
            data.disable_xwayland();
        }
    })?;
    Ok(())
}

/// Pump the loop until the X11 window manager is attached (or `timeout` elapses).
#[cfg(feature = "xwayland")]
pub fn pump_xwayland_ready(
    event_loop: &mut EventLoop<DesktopState>,
    display: &mut Display<DesktopState>,
    state: &mut DesktopState,
    timeout: Duration,
) -> anyhow::Result<bool> {
    let deadline = Instant::now() + timeout;
    while state.xwm.is_none() {
        if state.xwayland_client.is_none() {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                "XWayland client disconnected during startup"
            );
            return Ok(false);
        }
        if Instant::now() >= deadline {
            warn!(
                target: "focaldesk",
                session_id = session_id(),
                timeout_ms = timeout.as_millis(),
                "XWayland WM not ready before timeout"
            );
            return Ok(false);
        }
        // XWayland cannot finish startup until its Wayland client roundtrips with the compositor.
        display.dispatch_clients(state)?;
        crate::core::wayland::color_management_protocol::flush_pending_image_description_info_done(
            state,
        );
        display.handle().flush_clients()?;
        event_loop.dispatch(Some(Duration::ZERO), state)?;
    }
    Ok(true)
}

/// Block until the X11 WM is ready, or disable XWayland so the compositor can run without it.
#[cfg(feature = "xwayland")]
pub fn finish_xwayland_startup(
    event_loop: &mut EventLoop<DesktopState>,
    display: &mut Display<DesktopState>,
    state: &mut DesktopState,
    timeout: Duration,
) -> anyhow::Result<()> {
    if pump_xwayland_ready(event_loop, display, state, timeout)? {
        info!(
            target: "focaldesk",
            session_id = session_id(),
            "XWayland startup complete"
        );
    } else {
        state.disable_xwayland();
    }
    Ok(())
}

pub fn translate_backend_input<B: smithay::backend::input::InputBackend>(
    input: &InputEvent<B>,
    pointer_pos: Point<f64, Logical>,
    clamp_rect: Rectangle<i32, Logical>,
    _scale_factor: f64,
    modifiers: FlowModifiers,
) -> Option<FlowInputEvent> {
    match input {
        InputEvent::Keyboard { event, .. } => {
            let state = match event.state() {
                KeyState::Pressed => FlowKeyState::Pressed,
                KeyState::Released => FlowKeyState::Released,
            };

            Some(FlowInputEvent::Key {
                keycode: event.key_code().into(),
                state,
                repeat: false,
                modifiers,
            })
        }
        InputEvent::PointerMotion { event, .. } => {
            let pos = pointer_pos + event.delta();
            let delta_unaccel = event.delta_unaccel();
            let min_x = clamp_rect.loc.x as f64;
            let min_y = clamp_rect.loc.y as f64;
            let max_x = (clamp_rect.loc.x + clamp_rect.size.w) as f64 - f64::EPSILON;
            let max_y = (clamp_rect.loc.y + clamp_rect.size.h) as f64 - f64::EPSILON;

            Some(FlowInputEvent::PointerMoved {
                position: Point::from((
                    pos.x.clamp(min_x, max_x.max(min_x)),
                    pos.y.clamp(min_y, max_y.max(min_y)),
                )),
                delta: Some(event.delta()),
                delta_unaccel: Some(delta_unaccel),
            })
        }
        InputEvent::PointerMotionAbsolute { event, .. } => {
            let local = event.position_transformed(clamp_rect.size).to_f64();
            let pos = Point::<f64, Logical>::from((
                clamp_rect.loc.x as f64 + local.x,
                clamp_rect.loc.y as f64 + local.y,
            ));

            Some(FlowInputEvent::PointerMoved {
                position: pos,
                delta: None,
                delta_unaccel: None,
            })
        }
        InputEvent::PointerButton { event, .. } => {
            let button = match event.button_code() {
                0x110 => FlowMouseButton::Left,
                0x111 => FlowMouseButton::Right,
                0x112 => FlowMouseButton::Middle,
                0x113 => FlowMouseButton::Back,
                0x114 => FlowMouseButton::Forward,
                other => FlowMouseButton::Other(other as u16),
            };

            let state = match event.state() {
                ButtonState::Pressed => FlowKeyState::Pressed,
                ButtonState::Released => FlowKeyState::Released,
            };

            Some(FlowInputEvent::PointerButton {
                button,
                state,
                position: pointer_pos,
            })
        }
        InputEvent::PointerAxis { event, .. } => {
            let x_v120 = event
                .amount_v120(Axis::Horizontal)
                .map(|value| value as i32);
            let y_v120 = event.amount_v120(Axis::Vertical).map(|value| value as i32);
            let x = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                x_v120
                    .map(|value| f64::from(value) * 15.0 / 120.0)
                    .unwrap_or(0.0)
            });
            let y = event.amount(Axis::Vertical).unwrap_or_else(|| {
                y_v120
                    .map(|value| f64::from(value) * 15.0 / 120.0)
                    .unwrap_or(0.0)
            });
            let is_finger = event.source() == AxisSource::Finger;
            let source = match event.source() {
                AxisSource::Finger => FlowScrollSource::Finger,
                AxisSource::Continuous => FlowScrollSource::Continuous,
                AxisSource::Wheel => FlowScrollSource::Wheel,
                AxisSource::WheelTilt => FlowScrollSource::WheelTilt,
            };
            let delta = FlowScrollDelta::Axis {
                x,
                y,
                x_v120,
                y_v120,
                source,
                x_inverted: event.relative_direction(Axis::Horizontal)
                    == AxisRelativeDirection::Inverted,
                y_inverted: event.relative_direction(Axis::Vertical)
                    == AxisRelativeDirection::Inverted,
                stop_x: is_finger && event.amount(Axis::Horizontal) == Some(0.0),
                stop_y: is_finger && event.amount(Axis::Vertical) == Some(0.0),
            };

            Some(FlowInputEvent::PointerScroll {
                delta,
                position: pointer_pos,
            })
        }
        _ => None,
    }
}

/// Wayland socket + [`DesktopState`] and render helpers used by nested backend loops.
pub(crate) struct NestedDesktop {
    pub display: Display<DesktopState>,
    pub listener: ListeningSocket,
    pub wayland_display: String,
    pub state: DesktopState,
    pub clients: Vec<Client>,
    pub ui_state: UiState<GlesTexture>,
    pub scene: SceneState,
    pub output_state: OutputState,
    pub resources: RenderResources,
    pub start: Instant,
    pub last_now: Instant,
}

pub(crate) fn bind_wayland_socket() -> anyhow::Result<(ListeningSocket, String)> {
    let socket = ListeningSocket::bind_auto("focaldesk", 0..64)?;
    let name = socket
        .socket_name()
        .expect("FocalDesk: ListeningSocket has no name — bind_auto() should always provide one")
        .to_string_lossy()
        .to_string();
    Ok((socket, name))
}

fn shell_quote_for_xdpw_config(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn resolve_focaldesk_portal_executable() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("focaldesk-portal");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }

    for candidate in [
        "/usr/local/bin/focaldesk-portal",
        "/usr/bin/focaldesk-portal",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".local/bin/focaldesk-portal");
        if local.is_file() {
            return Some(local);
        }
    }

    None
}

/// xdpw reads `~/.config/xdg-desktop-portal-wlr/config` once; stale chooser paths break OBS capture.
fn ensure_xdpw_screencast_config() {
    let Some(portal_exe) = resolve_focaldesk_portal_executable() else {
        flog("focaldesk-portal not found; skipping xdpw screencast config update");
        return;
    };

    let config_dir = xdg_config_dir().join("xdg-desktop-portal-wlr");
    if let Err(err) = std::fs::create_dir_all(&config_dir) {
        flog(format!(
            "failed to create xdpw config directory {}: {err}",
            config_dir.display()
        ));
        return;
    }

    let config_path = config_dir.join("config");
    let chooser_cmd = shell_quote_for_xdpw_config(&portal_exe.to_string_lossy());
    let desired = format!("[screencast]\nchooser_type=simple\nchooser_cmd={chooser_cmd}\n");

    let current = std::fs::read_to_string(&config_path).unwrap_or_default();
    if current == desired {
        return;
    }

    match std::fs::write(&config_path, &desired) {
        Ok(()) => flog(format!(
            "updated xdpw screencast chooser to {}",
            portal_exe.display()
        )),
        Err(err) => flog(format!(
            "failed to write xdpw config {}: {err}",
            config_path.display()
        )),
    }
}

/// Start FocalDesk's session target after publishing the compositor environment.
/// The dedicated target owns the helper-service lifecycle without making
/// FocalDesk responsible for the shared `graphical-session.target`.
fn start_focaldesk_session_target(wayland_display: &str) {
    let import_status = std::process::Command::new("systemctl")
        .args([
            "--user",
            "import-environment",
            "WAYLAND_DISPLAY",
            "XDG_CURRENT_DESKTOP",
        ])
        .status();
    if let Err(err) = import_status {
        flog(format!(
            "failed to import session environment into systemd --user: systemctl: {err}"
        ));
    }

    let start_status = std::process::Command::new("systemctl")
        .args(["--user", "start", "--no-block", "focaldesk-session.target"])
        .status();
    match start_status {
        Ok(status) if status.success() => flog(format!(
            "started focaldesk-session.target for {wayland_display}"
        )),
        Ok(status) => flog(format!(
            "failed to start focaldesk-session.target: systemctl exited with {status}"
        )),
        Err(err) => flog(format!(
            "failed to start focaldesk-session.target: systemctl: {err}"
        )),
    }
}

/// Counterpart to [`start_focaldesk_session_target`], called once on clean compositor
/// exit (real DRM session only) so the per-domain helper daemons stop with the session
/// instead of being left running orphaned until the next login.
pub(crate) fn stop_focaldesk_session_target() {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "stop", "--no-block", "focaldesk-session.target"])
        .status();
    match status {
        Ok(status) if status.success() => flog("stopped focaldesk-session.target"),
        Ok(status) => flog(format!(
            "failed to stop focaldesk-session.target: systemctl exited with {status}"
        )),
        Err(err) => flog(format!(
            "failed to stop focaldesk-session.target: systemctl: {err}"
        )),
    }
}

fn publish_portal_environment(wayland_display: &str) {
    std::env::set_var("WAYLAND_DISPLAY", wayland_display);
    // The first component selects Focaldesk's portal routing (including the
    // location lockdown backend); wlroots remains as a compatibility fallback.
    std::env::set_var("XDG_CURRENT_DESKTOP", "focaldesk:wlroots");

    if std::env::var_os("FOCALDESK_DISABLE_PORTAL_ENV").is_some() {
        flog(format!(
            "portal environment publication disabled for {wayland_display}"
        ));
        return;
    }

    ensure_standard_user_dirs();
    ensure_xdpw_screencast_config();

    let mut environment_names = vec!["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP"];
    if std::env::var_os("FOCALDESK_PORTAL_COLOR").is_some() {
        environment_names.push("FOCALDESK_PORTAL_COLOR");
    }
    let status = std::process::Command::new("dbus-update-activation-environment")
        .arg("--systemd")
        .args(environment_names)
        .status();

    match status {
        Ok(status) if status.success() => {
            let portal_color = std::env::var("FOCALDESK_PORTAL_COLOR")
                .ok()
                .map(|value| format!(" FOCALDESK_PORTAL_COLOR={value}"))
                .unwrap_or_default();
            flog(format!(
                "published portal environment WAYLAND_DISPLAY={wayland_display} XDG_CURRENT_DESKTOP=focaldesk:wlroots{portal_color}"
            ));
        }
        Ok(status) => flog(format!(
            "failed to publish portal environment: dbus-update-activation-environment exited with {status}"
        )),
        Err(err) => flog(format!(
            "failed to publish portal environment: dbus-update-activation-environment: {err}"
        )),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn xdg_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn ensure_standard_user_dirs() {
    let Some(home) = home_dir() else {
        return;
    };

    for name in ["Desktop", "Downloads", "Music", "Pictures", "Videos"] {
        let path = home.join(name);
        if let Err(err) = std::fs::create_dir_all(&path) {
            flog(format!(
                "failed to create user directory {}: {err}",
                path.display()
            ));
        }
    }

    ensure_xdg_videos_dir();
}

fn ensure_xdg_videos_dir() {
    let user_dirs_path = xdg_config_dir().join("user-dirs.dirs");
    if let Some(parent) = user_dirs_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            flog(format!(
                "failed to create XDG user dirs config directory {}: {err}",
                parent.display()
            ));
            return;
        }
    }

    let videos_line = r#"XDG_VIDEOS_DIR="$HOME/Videos""#;
    let text = std::fs::read_to_string(&user_dirs_path).unwrap_or_default();
    let mut found = false;
    let mut changed = false;
    let mut lines = Vec::new();

    for line in text.lines() {
        if line.trim_start().starts_with("XDG_VIDEOS_DIR=") {
            found = true;
            if line.trim() == videos_line {
                lines.push(line.to_string());
            } else {
                lines.push(videos_line.to_string());
                changed = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if !found {
        lines.push(videos_line.to_string());
        changed = true;
    }

    if !changed {
        return;
    }

    let mut output = lines.join("\n");
    output.push('\n');
    if let Err(err) = std::fs::write(&user_dirs_path, output) {
        flog(format!(
            "failed to write XDG videos directory to {}: {err}",
            user_dirs_path.display()
        ));
    }
}

/// Restart portal services so xdg-desktop-portal-wlr reconnects and discovers current `wl_output`s.
/// xdpw only roundtrips the registry once at service start; outputs added later are invisible until restart.
fn restart_portal_services() {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", "xdg-desktop-portal-wlr.service"])
        .status();

    match status {
        Ok(status) if status.success() => {
            flog("reset failed state for xdg-desktop-portal-wlr.service")
        }
        Ok(status) => flog(format!(
            "failed to reset xdg-desktop-portal-wlr.service state: systemctl exited with {status}"
        )),
        Err(err) => flog(format!(
            "failed to reset xdg-desktop-portal-wlr.service state: systemctl: {err}"
        )),
    }

    let status = std::process::Command::new("systemctl")
        .args([
            "--user",
            "restart",
            "--no-block",
            "xdg-desktop-portal.service",
            "xdg-desktop-portal-wlr.service",
        ])
        .status();

    match status {
        Ok(status) if status.success() => {
            flog("requested async restart of xdg-desktop-portal and xdg-desktop-portal-wlr")
        }
        Ok(status) => flog(format!(
            "failed to restart xdg-desktop-portal services: systemctl exited with {status}"
        )),
        Err(err) => flog(format!(
            "failed to restart xdg-desktop-portal services: systemctl: {err}"
        )),
    }
}

/// Publish FocalDesk's client socket to portal services and restart them (best-effort).
pub(crate) fn refresh_portal_services(wayland_display: &str) {
    publish_portal_environment(wayland_display);
    if std::env::var_os("FOCALDESK_SKIP_PORTAL_RESTART").is_some() {
        flog("skipping portal service restart (FOCALDESK_SKIP_PORTAL_RESTART is set)");
        return;
    }
    restart_portal_services();
}

/// Build [`Display`], globals, and [`DesktopState`] for a nested output of the given size and scale.
pub(crate) fn bootstrap_compositor_core(
    bootstrap_output: Option<BootstrapOutput>,
    backend: BackendKind,
) -> anyhow::Result<NestedDesktop> {
    let (listener, wayland_display) = bind_wayland_socket()?;
    publish_portal_environment(&wayland_display);
    if backend == BackendKind::Drm {
        // Nested/winit dev sessions typically run inside an already-active desktop
        // session; only the real DRM-backed session should touch the shared
        // FocalDesk session target (the matching stop lives in backend::drm::run).
        start_focaldesk_session_target(&wayland_display);
    }
    flog(format!("FocalDesk client socket is {}", wayland_display));

    let display = Display::<DesktopState>::new()?;
    let dh = display.handle();

    let output = bootstrap_output.as_ref().map(|bootstrap_output| {
        let output = Output::new(
            bootstrap_output.name.clone(),
            PhysicalProperties {
                size: physical_size_mm_from_pixels(bootstrap_output.buffer_size).into(),
                subpixel: Subpixel::Unknown,
                make: "FocalDesk".into(),
                model: "Nested".into(),
                serial_number: "focaldesk-nested".into(),
            },
        );
        output.create_global::<DesktopState>(&dh);
        output
    });

    let compositor_state = CompositorState::new::<DesktopState>(&dh);
    let fractional_scale_manager_state =
        smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<DesktopState>(&dh);
    let viewporter_state = smithay::wayland::viewporter::ViewporterState::new::<DesktopState>(&dh);
    let xdg_shell_state = XdgShellState::new::<DesktopState>(&dh);
    let dmabuf_state = DmabufState::new();
    let shm_state = ShmState::new::<DesktopState>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<DesktopState>(&dh);
    let data_device_state = DataDeviceState::new::<DesktopState>(&dh);
    let primary_selection_state =
        smithay::wayland::selection::primary_selection::PrimarySelectionState::new::<DesktopState>(
            &dh,
        );
    let layer_shell_state =
        smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<DesktopState>(&dh);
    let image_capture_source_state =
        smithay::wayland::image_capture_source::ImageCaptureSourceState::new();
    let output_capture_source_state =
        smithay::wayland::image_capture_source::OutputCaptureSourceState::new::<DesktopState>(&dh);
    let image_copy_capture_state =
        smithay::wayland::image_copy_capture::ImageCopyCaptureState::new::<DesktopState>(&dh);
    crate::core::wayland::color_protocol::ColorTagState::bind_global::<DesktopState>(&dh);
    if crate::core::color::wp_color_management_enabled() {
        crate::core::wayland::color_management_protocol::ColorManagementState::bind_global::<
            DesktopState,
        >(&dh);
    }
    #[cfg(feature = "xwayland")]
    let xwayland_shell_state = XWaylandShellState::new::<DesktopState>(&dh);

    let settings = load_settings();

    let mut seat_state = smithay::input::SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat-0".to_string());
    seat.add_pointer();
    seat.add_keyboard(
        XkbConfig {
            layout: &settings.input.keyboard_layout,
            variant: &settings.input.keyboard_variant,
            model: &settings.input.keyboard_model,
            options: (!settings.input.keyboard_options.is_empty())
                .then(|| settings.input.keyboard_options.clone()),
            ..Default::default()
        },
        200,
        25,
    )?;

    let render = RenderState::new();
    let scale_factor = bootstrap_output
        .as_ref()
        .map(|output| output.scale_factor)
        .unwrap_or(1.0);
    let cursor_manager = CursorManager::new(24, scale_factor as f32);
    let notification_snapshots = Vec::<NotificationSnapshot>::new();
    let chrome = Chrome::new(ChromeMetrics::default());
    let xdg_activation_state = XdgActivationState::new::<DesktopState>(&dh);

    let config = FocalDeskConfig::load().unwrap_or_default();

    let theme_id = if config.appearance.theme.is_empty() {
        "Eagle".to_string()
    } else {
        config.appearance.theme.clone()
    };

    flog_info!("FOCALDESK selected theme_id = {:?}", theme_id);

    let theme_id = match config.appearance.theme.as_str() {
        "Eagle" => FlowThemeId::BuiltIn(BuiltInThemeId::Eagle),
        "Moonbase" => FlowThemeId::BuiltIn(BuiltInThemeId::Moonbase),
        "Classic" => FlowThemeId::BuiltIn(BuiltInThemeId::Classic),
        other => FlowThemeId::Custom(other.to_string()),
    };

    let theme_manager = ThemeManager::new(theme_id);
    let mut keybinds = Keybinds::with_defaults(backend);
    for warning in keybinds.apply_overrides(
        settings
            .input
            .keybindings
            .iter()
            .map(|(action, shortcut)| (action.as_str(), shortcut.as_str())),
    ) {
        warn!(target: "focaldesk", warning = %warning, "ignored keybinding setting");
    }

    flog_info!(
        "FOCALDESK active theme after manager init = {:?}",
        theme_manager.active_theme().id
    );

    let init = DesktopInit {
        display_handle: dh.clone(),
        xdg_activation_state,
        #[cfg(feature = "xwayland")]
        xwayland_shell_state,
        chrome,
        primary_output: OutputId(1),
        compositor_state,
        fractional_scale_manager_state,
        viewporter_state,
        render,
        xdg_shell_state,
        dmabuf_state,
        shm_state,
        seat_state,
        output_manager_state,
        data_device_state,
        primary_selection_state,
        pointer_constraints_state:
            smithay::wayland::pointer_constraints::PointerConstraintsState::new::<DesktopState>(&dh),
        relative_pointer_state:
            smithay::wayland::relative_pointer::RelativePointerManagerState::new::<DesktopState>(
                &dh,
            ),
        layer_shell_state,
        image_capture_source_state,
        output_capture_source_state,
        image_copy_capture_state,
        color_tag_state: Default::default(),
        color_management_state: Default::default(),
        cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState::new::<
            DesktopState,
        >(&dh),
        backend_kind: backend,
        cursor_manager,
        seat,
        notification_snapshots,
        keybinds,
        running: true,
        client_wayland_display: wayland_display.clone(),
        theme_manager,
        apps: settings.apps,
        workspaces: settings.workspaces,
        privacy: settings.privacy,
        power: settings.power,
        debug: settings.debug,
        chrome_items: settings.chrome,
    };

    let mut state = DesktopState::new(init);
    let output_state =
        if let (Some(output), Some(bootstrap_output)) = (output, bootstrap_output.as_ref()) {
            state.set_output_from_nested(
                output,
                bootstrap_output.buffer_size,
                bootstrap_output.scale_factor,
            );

            let desk_output = state
                .outputs
                .get(&state.primary_output)
                .expect("active output missing");

            OutputState::new_single_nested(
                (desk_output.logical_size.w, desk_output.logical_size.h),
                desk_output.scale_factor,
            )
        } else {
            OutputState::new_single_nested((1, 1), 1.0)
        };

    let start = Instant::now();
    let last_now = Instant::now();

    Ok(NestedDesktop {
        display,
        listener,
        wayland_display,
        state,
        clients: Vec::new(),
        ui_state: UiState::bootstrap(),
        scene: SceneState::new(),
        output_state,
        resources: RenderResources::new(),
        start,
        last_now,
    })
}
