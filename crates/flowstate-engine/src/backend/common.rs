//! Shared setup for nested compositor backends (winit, future DRM/KMS, etc.).

#[cfg(feature = "xwayland")]
use std::process::Stdio;
#[cfg(feature = "xwayland")]
use std::time::Duration;
use std::time::Instant;

use flowstate_cursor::CursorManager;
use flowstate_flow::Keybinds;
use flowstate_logging::flog;
use flowstate_notifications::NotificationManager;
use flowstate_resources::RenderResources;
use flowstate_types::OutputId;
use flowstate_ui::chrome::{Chrome, ChromeMetrics};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent,
    PointerMotionEvent,
};
use smithay::backend::renderer::gles::GlesTexture;
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

use crate::core::desktop::{DesktopInit, DesktopState};
use crate::core::input::{
    FlowInputEvent, FlowKeyState, FlowModifiers, FlowMouseButton, FlowScrollDelta, FlowScrollSource,
};
use crate::core::render::RenderState;
use crate::core::ui_state::UiState;
use crate::core::OutputState;
use crate::core::SceneState;
use flowstate_config::FlowStateConfig;
use flowstate_flow::keybinds::BackendKind;
use flowstate_settings_core::load_settings;
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::FlowThemeId;
use flowstate_themes::ThemeManager;
use smithay::wayland::xdg_activation::XdgActivationState;
#[cfg(feature = "xwayland")]
use smithay::wayland::xwayland_shell::XWaylandShellState;
#[cfg(feature = "xwayland")]
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};

/// Spawn XWayland and register it on `handle`. Sets `DISPLAY` once the server is up.
#[cfg(feature = "xwayland")]
pub fn start_xwayland(
    state: &mut DesktopState,
    display_handle: &DisplayHandle,
    handle: LoopHandle<'static, DesktopState>,
) -> anyhow::Result<()> {
    flog("XWAYLAND: preparing to launch XWayland server");
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
        true,
        Stdio::inherit(),
        Stdio::inherit(),
        |_| (),
    )?;
    state.xwayland_client = Some(xwayland_client.clone());
    let xwayland_display = format!(":{}", xwayland.display_number());
    state.xwayland_display = Some(xwayland_display.clone());
    std::env::set_var("DISPLAY", &xwayland_display);
    flog(&format!(
        "XWAYLAND: launched; reserved DISPLAY={xwayland_display}"
    ));

    let wm_handle = handle.clone();
    let wm_dh = display_handle.clone();
    handle.insert_source(xwayland, move |event, _, data| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            let display = format!(":{display_number}");
            data.xwayland_display = Some(display.clone());
            std::env::set_var("DISPLAY", &display);
            flog(&format!(
                "XWAYLAND READY: display={display} socket_fd={x11_socket:?}"
            ));

            match X11Wm::start_wm(
                wm_handle.clone(),
                &wm_dh,
                x11_socket,
                xwayland_client.clone(),
            ) {
                Ok(wm) => {
                    data.xwm = Some(wm);
                    flog(&format!("XWayland WM attached on DISPLAY={display}"));
                }
                Err(err) => {
                    flog(&format!("failed to start XWayland WM: {err}"));
                    data.disable_xwayland();
                }
            }
        }
        XWaylandEvent::Error => {
            flog("XWayland failed to start");
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
            flog("XWayland client disconnected during startup");
            return Ok(false);
        }
        if Instant::now() >= deadline {
            flog("XWayland WM not ready before timeout");
            return Ok(false);
        }
        // XWayland cannot finish startup until its Wayland client roundtrips with the compositor.
        display.dispatch_clients(state)?;
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
        flog("XWayland startup complete");
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
            let min_x = clamp_rect.loc.x as f64;
            let min_y = clamp_rect.loc.y as f64;
            let max_x = (clamp_rect.loc.x + clamp_rect.size.w) as f64 - f64::EPSILON;
            let max_y = (clamp_rect.loc.y + clamp_rect.size.h) as f64 - f64::EPSILON;

            Some(FlowInputEvent::PointerMoved {
                position: Point::from((
                    pos.x.clamp(min_x, max_x.max(min_x)),
                    pos.y.clamp(min_y, max_y.max(min_y)),
                )),
            })
        }
        InputEvent::PointerMotionAbsolute { event, .. } => {
            let local = event.position_transformed(clamp_rect.size).to_f64();
            let pos = Point::<f64, Logical>::from((
                clamp_rect.loc.x as f64 + local.x,
                clamp_rect.loc.y as f64 + local.y,
            ));

            Some(FlowInputEvent::PointerMoved { position: pos })
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
    let socket = ListeningSocket::bind_auto("flowstate", 0..64)?;
    let name = socket
        .socket_name()
        .expect("Flowstate: ListeningSocket has no name — bind_auto() should always provide one")
        .to_string_lossy()
        .to_string();
    Ok((socket, name))
}

/// Build [`Display`], globals, and [`DesktopState`] for a nested output of the given size and scale.
pub(crate) fn bootstrap_compositor_core(
    output_name: String,
    buffer_size: Size<i32, Physical>,
    scale_factor: f64,
    backend: BackendKind,
) -> anyhow::Result<NestedDesktop> {
    let (listener, wayland_display) = bind_wayland_socket()?;
    std::env::set_var("WAYLAND_DISPLAY", &wayland_display);
    flog(&format!("FlowState client socket is {}", wayland_display));

    let mut display = Display::<DesktopState>::new()?;
    let dh = display.handle();

    let output = Output::new(
        output_name,
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "FlowState".into(),
            model: "Nested".into(),
            serial_number: "flowstate-nested".into(),
        },
    );
    output.create_global::<DesktopState>(&dh);

    let compositor_state = CompositorState::new::<DesktopState>(&dh);
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
    #[cfg(feature = "xwayland")]
    let xwayland_shell_state = XWaylandShellState::new::<DesktopState>(&dh);

    let mut seat_state = smithay::input::SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat-0".to_string());
    seat.add_pointer();
    seat.add_keyboard(Default::default(), 200, 25)?;

    let render = RenderState::new();
    let cursor_manager = CursorManager::new(24, scale_factor as f32);
    let notifications = NotificationManager::new();
    let chrome = Chrome::new(ChromeMetrics::default());
    let xdg_activation_state = XdgActivationState::new::<DesktopState>(&dh);

    let config = FlowStateConfig::load().unwrap_or_default();
    let settings = load_settings();

    let theme_id = if config.appearance.theme.is_empty() {
        "Eagle".to_string()
    } else {
        config.appearance.theme.clone()
    };

    eprintln!("FLOWSTATE selected theme_id = {:?}", theme_id);

    let theme_id = match config.appearance.theme.as_str() {
        "Eagle" => FlowThemeId::BuiltIn(BuiltInThemeId::Eagle),
        "Moonbase" => FlowThemeId::BuiltIn(BuiltInThemeId::Moonbase),
        "Classic" => FlowThemeId::BuiltIn(BuiltInThemeId::Classic),
        other => FlowThemeId::Custom(other.to_string()),
    };

    let theme_manager = ThemeManager::new(theme_id);

    eprintln!(
        "FLOWSTATE active theme after manager init = {:?}",
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
        render,
        xdg_shell_state,
        dmabuf_state,
        shm_state,
        seat_state,
        output_manager_state,
        data_device_state,
        primary_selection_state,
        layer_shell_state,
        image_capture_source_state,
        output_capture_source_state,
        image_copy_capture_state,
        backend_kind: backend,
        cursor_manager,
        seat,
        notifications,
        keybinds: Keybinds::default(),
        running: true,
        client_wayland_display: wayland_display.clone(),
        theme_manager,
        apps: settings.apps,
    };

    let mut state = DesktopState::new(init);
    state.set_output_from_nested(output.clone(), buffer_size, scale_factor);

    let desk_output = state
        .outputs
        .get(&state.primary_output)
        .expect("active output missing");

    let output_state = OutputState::new_single_nested(
        (desk_output.logical_size.w, desk_output.logical_size.h),
        desk_output.scale_factor,
    );

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
