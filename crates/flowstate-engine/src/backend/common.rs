//! Shared setup for nested compositor backends (winit, future DRM/KMS, etc.).

use std::time::Instant;

use flowstate_cursor::CursorManager;
use flowstate_flow::Keybinds;
use flowstate_logging::flog;
use flowstate_notifications::NotificationManager;
use flowstate_types::OutputId;
use flowstate_ui::chrome::{Chrome, ChromeMetrics};
use flowstate_resources::RenderResources;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::output::{Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::utils::{Physical, Size};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::core::desktop::{DesktopInit, DesktopState};
use crate::core::render::RenderState;
use crate::core::ui_state::UiState;
use crate::core::OutputState;
use crate::core::SceneState;
use flowstate_flow::keybinds::BackendKind;
use smithay::wayland::xdg_activation::XdgActivationState;
use flowstate_themes::FlowThemeId;
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::ThemeManager;
use flowstate_config::FlowConfig;


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
    let shm_state = ShmState::new::<DesktopState>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<DesktopState>(&dh);
    let data_device_state = DataDeviceState::new::<DesktopState>(&dh);
    let layer_shell_state =
        smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<DesktopState>(&dh);
    let image_capture_source_state =
        smithay::wayland::image_capture_source::ImageCaptureSourceState::new();
    let output_capture_source_state =
        smithay::wayland::image_capture_source::OutputCaptureSourceState::new::<DesktopState>(&dh);
    let image_copy_capture_state =
        smithay::wayland::image_copy_capture::ImageCopyCaptureState::new::<DesktopState>(&dh);

    let mut seat_state = smithay::input::SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat-0".to_string());
    seat.add_pointer();
    seat.add_keyboard(Default::default(), 200, 25)?;

    let render = RenderState::new();
    let cursor_manager = CursorManager::new(24, scale_factor as f32);
    let notifications = NotificationManager::new();
    let chrome = Chrome::new(ChromeMetrics::default());
    let xdg_activation_state = XdgActivationState::new::<DesktopState>(&dh);
    
    let config = FlowConfig::load().unwrap_or_default();

    let theme_id = config
        .theme
        .active
        .unwrap_or(FlowThemeId::BuiltIn(BuiltInThemeId::Eagle));
        
   eprintln!("FLOWSTATE selected theme_id = {:?}", theme_id);
 

    let theme_manager = ThemeManager::new(theme_id);
    
    eprintln!(
    "FLOWSTATE active theme after manager init = {:?}",
    theme_manager.active_theme().id
);

    let init = DesktopInit {
        xdg_activation_state,
        chrome,
        primary_output: OutputId(1),
        compositor_state,
        render,
        xdg_shell_state,
        shm_state,
        seat_state,
        output_manager_state,
        data_device_state,
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
