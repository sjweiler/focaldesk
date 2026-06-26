#![allow(unused_imports)]

use focaldesk_types::types::{OutputId, WindowId, WorkspaceId};
use focaldesk_ui::uitree::UiTree;
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::renderer::utils::import_surface_tree;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, PopupKind, PopupManager, Space, Window,
};
use smithay::wayland::compositor::get_parent;
use smithay::wayland::compositor::is_sync_subsurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::core::color::{
    effective_surface_render_state, force_linear_surfaces, ColorDescription,
    RenderingIntent, SurfaceColorRenderState, SurfaceColorState,
};
use crate::core::output_store::OutputStore;
use crate::core::window_store::WindowStore;
use crate::core::workspace_store::WorkspaceStore;
use focaldesk_ui::desktop_frame::DesktopFrameCtx;
use focaldesk_ui::egui_layer::{EguiInputEvent, EguiModifiers, EguiPointerButton, EguiScrollDelta};
use focaldesk_ui::element::UiElement;
use focaldesk_ui::types::{ElementId, PanelKind, UiAction, UiElementKind};
use smithay::backend::input::{Axis, AxisRelativeDirection, AxisSource, ButtonState};
use smithay::backend::renderer::element::Id;
use smithay::desktop::{WindowSurface, WindowSurfaceType};
use smithay::input::keyboard::{keysyms, xkb};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorIcon, MotionEvent};
use smithay::reexports::wayland_server::Resource;

use crate::core::shell::xwayland::{XwaylandSurfaceRole, XwaylandWindowMeta};
use crate::core::shell::WaylandWindowMeta;
use focaldesk_cursor::{CursorIcon as FlowCursorIcon, CursorManager};
use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
#[cfg(feature = "xwayland")]
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::core::input::FlowKeyState;
use crate::core::input::FlowModifiers;
use crate::core::input::FlowMouseButton;
use crate::core::input::FlowScrollDelta;
use crate::core::input::FlowScrollSource;
use crate::core::input::{FlowInputEvent, InputState};
use crate::core::lock::{
    authenticate_current_user, LockPulseKind, LockScreenState, LOCK_PULSE_DURATION,
};
use crate::core::shell::ManagedWindow;
use crate::core::RenderState;
use focaldesk_config::{load_config, save_config, FocalDeskConfig};
use focaldesk_flow::actions::KeyAction;
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_flow::Keybinds;
use focaldesk_flow::ModMask;
use focaldesk_ipc::{DisplayRuntimeOutputStatus, IpcRequest, IpcResponse, DESKTOP_SOCKET_PATH};
use focaldesk_logging::session_id;
use focaldesk_logging::{
    flog, flog_error, flog_info, flog_warn, log_file_path_candidates, set_log_level, FLogLevel,
};
use focaldesk_notifications::NotificationManager;
use focaldesk_settings_core::{
    load_settings, AppSettings, BrowserLaunchBackend, DebugLogLevel, DebugSettings, OutputConfig,
    PrivacySettings,
};
use focaldesk_sounds::{UiSound, UiSoundPlayer};
use focaldesk_ui::chrome::Chrome;
use focaldesk_ui::chrome::ChromeMetrics;
use indexmap::IndexMap;
use smithay::delegate_dispatch2;
use smithay::input::keyboard::FilterResult;
use smithay::input::Seat;
use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScaleSmithay, Subpixel};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::utils::Serial;
use smithay::utils::SERIAL_COUNTER;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::PopupSurface;
use smithay::wayland::shell::xdg::ToplevelSurface;
use std::path::{Path, PathBuf};
use std::process::id;
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::{debug, info_span, trace};
use tracing_subscriber::fmt::time;
use wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge};

use smithay::wayland::compositor;
use smithay::wayland::compositor::SurfaceAttributes;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::wayland::xdg_activation::XdgActivationState;
#[cfg(feature = "xwayland")]
use smithay::wayland::xwayland_shell::XWaylandShellState;
#[cfg(feature = "xwayland")]
use smithay::xwayland::X11Wm;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use wayland_server::DisplayHandle;

use crate::core::chrome_layout::{
    build_chrome_layout, chrome_host_drag_hit, sidebar_slot_index_at, topbar_status_well_index_at,
};
use crate::core::focus::{KeyboardFocusTarget, PointerFocusTarget};
use crate::core::fonts::FontSystem;
use crate::core::toplevel_interaction::{
    cursor_for_resize_edges, handle_resize_surface_commit, resize_edges_at, ResizeEdgeMask,
    ResizeSurfaceState, ToplevelPointerInteraction, RESIZE_BORDER_PX,
};
use crate::core::ui_builder::{build_ui_for_output_with_options, AiFlowMode, UiBuildOptions};
use focaldesk_ai::{send_ai_request, AiDaemonStatus, AiIpcRequest, AiIpcResponse};
use focaldesk_themes::theme::BuiltInThemeId;
use focaldesk_themes::FlowThemeId;
use focaldesk_themes::ThemeManager;
use focaldesk_ui::ai_permission::AiPermissionDialog;
use focaldesk_ui::dialog::DialogAction;
use focaldesk_ui::dialog::{Dialog, DialogId};
use focaldesk_ui::dialog_layout::layout_dialog;
use focaldesk_ui::ui_builder::{
    SIDEBAR_ADD_WORKSPACE_ID, SIDEBAR_BROWSER_ID, SIDEBAR_DELETE_WORKSPACE_ID, SIDEBAR_FILES_ID,
    SIDEBAR_SETTINGS_ID, SIDEBAR_TERMINAL_ID, SIDEBAR_WORKSPACE_1_ID, SIDEBAR_WORKSPACE_2_ID,
    SIDEBAR_WORKSPACE_3_ID,
};

fn clamp_rect_to_bounds(
    mut geometry: Rectangle<i32, Logical>,
    bounds: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    if bounds.size.w <= 0 || bounds.size.h <= 0 {
        return geometry;
    }

    geometry.size.w = geometry.size.w.clamp(1, bounds.size.w);
    geometry.size.h = geometry.size.h.clamp(1, bounds.size.h);

    let max_x = (bounds.loc.x + bounds.size.w - geometry.size.w).max(bounds.loc.x);
    let max_y = (bounds.loc.y + bounds.size.h - geometry.size.h).max(bounds.loc.y);

    geometry.loc.x = geometry.loc.x.clamp(bounds.loc.x, max_x);
    geometry.loc.y = geometry.loc.y.clamp(bounds.loc.y, max_y);
    geometry
}

pub(crate) const DND_CURSOR_ENDED: u8 = 0;
pub(crate) const DND_CURSOR_FILE: u8 = 1;
pub(crate) const DND_CURSOR_VALID: u8 = 2;
pub(crate) const DND_CURSOR_INVALID: u8 = 3;

pub(crate) fn dbg_flush(msg: &str) {
    if !focaldesk_logging::enabled(FLogLevel::Debug) {
        return;
    }

    tracing::debug!(
        target: "focaldesk",
        session_id = session_id(),
        message = %msg,
        "dbg_flush"
    );
}

fn start_desktop_settings_ipc() -> mpsc::Receiver<DesktopIpcMessage> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let _ = std::fs::remove_file(DESKTOP_SOCKET_PATH);
        let listener = match UnixListener::bind(DESKTOP_SOCKET_PATH) {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(
                    target: "focaldesk",
                    session_id = session_id(),
                    path = %DESKTOP_SOCKET_PATH,
                    error = %err,
                    "failed to bind desktop settings IPC socket"
                );
                return;
            }
        };

        tracing::info!(
            target: "focaldesk",
            session_id = session_id(),
            path = %DESKTOP_SOCKET_PATH,
            "desktop settings IPC listening"
        );
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let tx = tx.clone();
                    thread::spawn(move || handle_desktop_settings_ipc_stream(&mut stream, &tx));
                }
                Err(err) => {
                    tracing::warn!(
                        target: "focaldesk",
                        session_id = session_id(),
                        path = %DESKTOP_SOCKET_PATH,
                        error = %err,
                        "desktop settings IPC accept failed"
                    );
                }
            }
        }
    });

    rx
}

fn handle_desktop_settings_ipc_stream(
    stream: &mut UnixStream,
    tx: &mpsc::Sender<DesktopIpcMessage>,
) {
    let mut payload = String::new();
    if let Err(err) = stream.read_to_string(&mut payload) {
        write_ipc_response(
            stream,
            IpcResponse::Error {
                message: err.to_string(),
            },
        );
        return;
    }

    let request = match serde_json::from_str::<IpcRequest>(&payload) {
        Ok(request) => request,
        Err(err) => {
            write_ipc_response(
                stream,
                IpcResponse::Error {
                    message: err.to_string(),
                },
            );
            return;
        }
    };

    let (response_tx, response_rx) = mpsc::channel();
    let is_watch = matches!(request, IpcRequest::Watch { .. });
    if tx
        .send(DesktopIpcMessage::Request {
            request,
            response: response_tx,
        })
        .is_err()
    {
        write_ipc_response(
            stream,
            IpcResponse::Error {
                message: "desktop settings IPC handler is unavailable".to_string(),
            },
        );
        return;
    }

    if is_watch {
        for response in response_rx {
            write_ipc_response(stream, response);
        }
        return;
    }

    match response_rx.recv() {
        Ok(response) => write_ipc_response(stream, response),
        Err(err) => write_ipc_response(
            stream,
            IpcResponse::Error {
                message: err.to_string(),
            },
        ),
    }
}

fn write_ipc_response(stream: &mut UnixStream, response: IpcResponse) {
    if let Ok(json) = serde_json::to_vec(&response) {
        let _ = stream.write_all(&json);
        let _ = stream.write_all(b"\n");
    }
}

fn theme_id_from_config(config: &FocalDeskConfig) -> FlowThemeId {
    match config.appearance.theme.as_str() {
        "Eagle" | "" => FlowThemeId::BuiltIn(BuiltInThemeId::Eagle),
        "Moonbase" => FlowThemeId::BuiltIn(BuiltInThemeId::Moonbase),
        "Classic" => FlowThemeId::BuiltIn(BuiltInThemeId::Classic),
        other => FlowThemeId::Custom(other.to_string()),
    }
}

fn get_config_key(config: &FocalDeskConfig, key: &str) -> Option<serde_json::Value> {
    let value = serde_json::to_value(config).ok()?;
    get_json_key(&value, key).cloned()
}

fn get_json_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for part in key.split('.') {
        if part.is_empty() {
            return None;
        }
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

fn runtime_display_status_key() -> &'static str {
    "displays.runtime"
}

fn runtime_display_status_value(state: &DesktopState) -> serde_json::Value {
    let outputs: Vec<DisplayRuntimeOutputStatus> = state
        .outputs
        .values()
        .map(|output| DisplayRuntimeOutputStatus {
            connector: output.handle.name().to_string(),
            icc_lut_fallback_active: output.icc_lut_fallback_active,
        })
        .collect();
    serde_json::to_value(outputs).unwrap_or(serde_json::Value::Null)
}

fn get_runtime_key(state: &DesktopState, key: &str) -> Option<serde_json::Value> {
    if key == runtime_display_status_key() {
        return Some(runtime_display_status_value(state));
    }
    None
}

fn set_json_key(
    value: &mut serde_json::Value,
    key: &str,
    new_value: serde_json::Value,
) -> Result<(), String> {
    let mut parts = key.split('.').peekable();
    let mut current = value;

    while let Some(part) = parts.next() {
        if part.is_empty() {
            return Err("config key contains an empty segment".to_string());
        }

        if parts.peek().is_none() {
            let object = current
                .as_object_mut()
                .ok_or_else(|| format!("config key is not an object path: {key}"))?;
            if !object.contains_key(part) {
                return Err(format!("unknown config key: {key}"));
            }
            object.insert(part.to_string(), new_value);
            return Ok(());
        }

        current = current
            .as_object_mut()
            .and_then(|object| object.get_mut(part))
            .ok_or_else(|| format!("unknown config key: {key}"))?;
    }

    Err("config key is empty".to_string())
}

pub struct OutputState {
    pub handle: Output,
    pub physical_size: Size<i32, Physical>,
    pub logical_size: Size<i32, Logical>,
    pub logical_origin: Point<i32, Logical>,
    pub scale_factor: f64,
    pub scale: Scale<f64>,
    pub hdr_supported: bool,
    pub hdr_requested: bool,
    pub hdr_enabled: bool,
    /// EDID Type-1 HDR static metadata (nits), when detected.
    pub edid_hdr_max_luminance_nits: Option<f32>,
    pub edid_hdr_max_fall_nits: Option<f32>,
    pub active_workspace: WorkspaceId,
    pub pending_damage: Vec<Rectangle<i32, Physical>>,
    pub last_sw_cursor_rect: Option<Rectangle<i32, Physical>>,
    pub color_description: crate::core::color::ColorDescription,
    pub icc_profile: Option<Vec<u8>>,
    pub output_icc_lut: Option<crate::core::icc_lut::OutputIccLut>,
    pub icc_lut_fallback_active: bool,
    pub monitor_make: String,
    pub monitor_model: String,
    pub monitor_serial: String,
    pub monitor_edid: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub enum DamageSource {
    WindowMove,
    WindowResize,
    Cursor,
    Hover,
    CommitBbox,
    FullRedrawFallback,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DamageSourceCounts {
    pub window_move: u64,
    pub window_resize: u64,
    pub cursor: u64,
    pub hover: u64,
    pub commit_bbox: u64,
    pub full_redraw_fallback: u64,
    pub unknown: u64,
}

impl DamageSourceCounts {
    fn record(&mut self, source: DamageSource) {
        match source {
            DamageSource::WindowMove => self.window_move += 1,
            DamageSource::WindowResize => self.window_resize += 1,
            DamageSource::Cursor => self.cursor += 1,
            DamageSource::Hover => self.hover += 1,
            DamageSource::CommitBbox => self.commit_bbox += 1,
            DamageSource::FullRedrawFallback => self.full_redraw_fallback += 1,
            DamageSource::Unknown => self.unknown += 1,
        }
    }
}

pub struct DesktopInit {
    pub display_handle: DisplayHandle,
    pub xdg_activation_state: XdgActivationState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: XWaylandShellState,
    pub primary_output: OutputId,
    pub running: bool,
    pub compositor_state: CompositorState,
    pub render: RenderState,
    pub xdg_shell_state: XdgShellState,
    pub dmabuf_state: DmabufState,
    pub shm_state: ShmState,
    pub seat_state: smithay::input::SeatState<DesktopState>,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub layer_shell_state: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_source_state:
        smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub image_copy_capture_state: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    pub color_tag_state: crate::core::wayland::color_protocol::ColorTagState,
    pub color_management_state:
        crate::core::wayland::color_management_protocol::ColorManagementState,
    pub backend_kind: BackendKind,
    pub cursor_manager: CursorManager,
    pub seat: Seat<DesktopState>,
    pub notifications: NotificationManager,
    pub chrome: focaldesk_ui::chrome::Chrome,
    pub keybinds: Keybinds,
    pub client_wayland_display: String,
    pub theme_manager: ThemeManager,
    pub apps: AppSettings,
    pub privacy: PrivacySettings,
    pub debug: DebugSettings,
}

enum DesktopIpcMessage {
    Request {
        request: IpcRequest,
        response: mpsc::Sender<IpcResponse>,
    },
}

struct DesktopIpcWatcher {
    keys: Vec<String>,
    response: mpsc::Sender<IpcResponse>,
}

pub struct DesktopState {
    // smithay protocol state
    pub display_handle: DisplayHandle,
    pub xdg_activation_state: XdgActivationState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: XWaylandShellState,
    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xwayland_client: Option<smithay::reexports::wayland_server::Client>,
    #[cfg(feature = "xwayland")]
    pub xwayland_display: Option<String>,
    #[cfg(feature = "xwayland")]
    pub xwayland_loop_handle: Option<LoopHandle<'static, DesktopState>>,
    pub winit_scale_factor: f64,
    pub ui: UiTree,
    pub active_workspace: WorkspaceId,
    pub workspace_names: Vec<String>,
    pub next_window_id: WindowId,
    pub primary_output: OutputId,
    pub focused_output: OutputId, //keyboard shit
    pub focus_changed_at: Instant,
    pub input: InputState,
    // pub keybinds: Keybinds,
    pub running: bool,
    pub compositor_state: CompositorState,
    pub render: RenderState,
    pub xdg_shell_state: smithay::wayland::shell::xdg::XdgShellState,
    pub dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub dmabuf_node: Option<smithay::backend::drm::DrmNode>,
    pub portal_dmabuf_formats: Vec<(Fourcc, Vec<Modifier>)>,
    pub shm_state: smithay::wayland::shm::ShmState,
    pub seat_state: smithay::input::SeatState<Self>,
    pub output_manager_state: smithay::wayland::output::OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub layer_shell_state: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_source_state:
        smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub image_copy_capture_state: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    pub image_copy_capture_sessions: Vec<smithay::wayland::image_copy_capture::Session>,
    pub color_tag_state: crate::core::wayland::color_protocol::ColorTagState,
    pub color_management_state:
        crate::core::wayland::color_management_protocol::ColorManagementState,
    pub portal_dispatch_ctx: Option<crate::core::portal::PortalDispatchCtx>,
    pub pending_portal_captures: Vec<crate::core::portal::PendingPortalCapture>,
    pending_ai_permission_responses: HashMap<DialogId, (u64, mpsc::Sender<IpcResponse>)>,
    pub portal_frame_cache: HashMap<OutputId, crate::core::portal::PortalFrameCache>,
    /// Latest DRM offscreen texture per output for portal/OBS capture.
    pub portal_capture_source: HashMap<OutputId, crate::core::portal::PortalCaptureSource>,
    /// Offscreen targets for portal re-render fallback (matches linear/legacy scanout path).
    pub portal_offscreen_targets:
        HashMap<OutputId, crate::core::linear_compositing::LinearOffscreenTargets>,
    /// Set after the first successful DRM present; portal capture waits for this.
    pub compositor_ready: bool,
    pub backend_kind: BackendKind,
    pub cursor_manager: CursorManager,
    pub seat: Seat<DesktopState>,
    // desktop model
    pub space: Space<Window>,
    pub popups: PopupManager,
    pub windows: Vec<ManagedWindow>,
    pub dialogs: Vec<Dialog>,
    pub active_dialog: Option<DialogId>,
    pub outputs: IndexMap<OutputId, OutputState>,
    pub current_workspace: u64,
    pub chrome: focaldesk_ui::chrome::Chrome,
    // focus/input
    pub seat_name: String,
    pub focused_window: Option<WindowId>,
    pub pointer_pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    /// In-progress interactive XDG move/resize driven by nested (winit) pointer events.
    pub toplevel_pointer: Option<ToplevelPointerInteraction>,
    pub(crate) dnd_cursor_phase: Option<Arc<AtomicU8>>,

    // shell/chrome
    //pub topbar: TopBarModel,
    //pub sidebar: SidebarModel,
    pub notifications: NotificationManager,
    pub lock_screen: LockScreenState,

    // xwayland and special surfaces
    pub unmapped_windows: Vec<ManagedWindow>,

    pub keybinds: Keybinds,

    pub client_wayland_display: String,
    pub apps: AppSettings,
    pub privacy: PrivacySettings,
    pub debug: DebugSettings,
    settings_ipc_rx: mpsc::Receiver<DesktopIpcMessage>,
    settings_ipc_watchers: Vec<DesktopIpcWatcher>,
    settings_ipc_config: FocalDeskConfig,

    /// Undecorated winit window: set on left-press over chrome top bar; backend calls platform window drag.
    host_window_drag_requested: bool,

    /// Left press on a client in the work area: after pointer moves past a threshold, start compositor move.
    pending_compositor_move: Option<(WindowId, Point<f64, Logical>)>,

    /// GTK/Wayland titlebar drag: `xdg_toplevel.move` is deferred until the pointer crosses a threshold
    /// so a simple click still reaches the client (immediate compositor grab blocks forwarding).
    pending_xdg_move: Option<(WindowId, Point<f64, Logical>)>,

    /// Last compositor-managed titlebar click, used for XWayland double-click maximize.
    last_titlebar_click: Option<(WindowId, Instant, Point<f64, Logical>)>,
    suppress_next_left_release: bool,

    /// Stable [`Id`] for the DRM cursor [`TextureRenderElement`] so [`RenderElementStates`] can be inspected.
    pub drm_cursor_render_id: Id,
    /// When true, pass a separate `Kind::Cursor` element to [`smithay::backend::drm::DrmOutput::render_frame`].
    pub drm_submit_hw_cursor: bool,
    /// One frame: attempt a separate DRM cursor element while suppressing the in-buffer software draw.
    pub drm_try_pass_cursor_this_frame: bool,
    /// Output that most recently owned cursor presentation. Used to force a cleanup frame
    /// on the old DRM output when the pointer crosses outputs.
    cursor_owner_output: Option<OutputId>,

    pub screenshot_requested: Option<OutputId>,
    pub screenshot_all_requested: bool,
    pub screenshot_seq: u64,

    pub fonts: FontSystem,

    pub theme: ThemeManager,
    /// Latest committed color state per Wayland surface render id.
    pub surface_colors: HashMap<Id, SurfaceColorRenderState>,
    pub damage_debug_enabled: bool,
    pub damage_source_counts: DamageSourceCounts,
    pub sidebar_pulse: Option<SidebarPulse>,
    pub topbar_pulse: Option<TopbarPulse>,
    pub flow_field_pulse: Option<FlowFieldPulse>,
    pub clock_pulse: Option<ClockPulse>,
    ai_flow_mode_cache: AiFlowMode,
    ai_flow_mode_last_poll: Instant,
    pub ui_sound_player: UiSoundPlayer,
    last_sidebar_hover_sound_target: Option<(OutputId, ElementId)>,
    last_clock_text: String,
    next_dialog_id: DialogId,
    //pub popups: Vec<PopupState>,
}

pub(crate) const SIDEBAR_PULSE_DURATION: Duration = Duration::from_millis(700);
pub(crate) const TOPBAR_PULSE_DURATION: Duration = SIDEBAR_PULSE_DURATION;
pub(crate) const FLOW_FIELD_PULSE_DURATION: Duration = SIDEBAR_PULSE_DURATION;
pub(crate) const CLOCK_PULSE_DURATION: Duration = SIDEBAR_PULSE_DURATION;

fn apply_debug_log_level(level: DebugLogLevel) {
    let level = match level {
        DebugLogLevel::Error => FLogLevel::Error,
        DebugLogLevel::Warn => FLogLevel::Warn,
        DebugLogLevel::Info => FLogLevel::Info,
        DebugLogLevel::Debug => FLogLevel::Debug,
        DebugLogLevel::Trace => FLogLevel::Trace,
    };
    set_log_level(level);
}

fn debug_damage_enabled(debug: &DebugSettings) -> bool {
    debug.show_damage_regions
        || std::env::var("FOCALDESK_DAMAGE_DEBUG")
            .is_ok_and(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
}

#[derive(Clone, Copy, Debug)]
pub struct SidebarPulse {
    pub output_id: OutputId,
    pub slot: usize,
    pub click_local: Point<f64, Logical>,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct SidebarPulseFrame {
    pub slot: usize,
    pub click_local: Point<f64, Logical>,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct TopbarPulse {
    pub output_id: OutputId,
    pub indicator: usize,
    pub click_local: Point<f64, Logical>,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct TopbarPulseFrame {
    pub indicator: usize,
    pub click_local: Point<f64, Logical>,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct FlowFieldPulse {
    pub output_id: OutputId,
    pub click_local: Point<f64, Logical>,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct FlowFieldPulseFrame {
    pub click_local: Point<f64, Logical>,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct ClockPulse {
    pub output_id: OutputId,
    pub click_local: Point<f64, Logical>,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct ClockPulseFrame {
    pub click_local: Point<f64, Logical>,
    pub elapsed: Duration,
}

impl DesktopState {
    pub fn process_settings_ipc_requests(&mut self) {
        while let Ok(message) = self.settings_ipc_rx.try_recv() {
            match message {
                DesktopIpcMessage::Request { request, response } => {
                    if let Some(response_value) =
                        self.handle_settings_ipc_request(request, response.clone())
                    {
                        let _ = response.send(response_value);
                    }
                }
            }
        }
    }

    pub fn process_chrome_timers(&mut self) {
        let clock_text = chrono::Local::now().format("%-I:%M %p").to_string();
        if self.last_clock_text == clock_text {
            return;
        }

        self.last_clock_text = clock_text;
        self.mark_all_outputs_clock_damage(DamageSource::Unknown);
    }

    pub fn process_notification_timers(&mut self) {
        let now = Instant::now();
        let had_visible = self.notifications.has_visible(now);
        let expired = self.notifications.expire(now);

        if had_visible || expired {
            self.mark_focused_output_full_damage(DamageSource::Unknown);
        }
    }

    pub fn process_lock_timers(&mut self) {
        if !self.lock_screen.active {
            return;
        }

        let now = Instant::now();
        if let Some(pulse) = self.lock_screen.pulse {
            let elapsed = now.saturating_duration_since(pulse.started_at);
            if pulse.kind == LockPulseKind::Accepted && elapsed >= LOCK_PULSE_DURATION {
                self.lock_screen.unlock();
                self.mark_all_outputs_full_damage(DamageSource::Unknown);
                return;
            }

            if elapsed < LOCK_PULSE_DURATION {
                self.mark_all_outputs_full_damage(DamageSource::Unknown);
            }
        }
    }

    fn handle_settings_ipc_request(
        &mut self,
        request: IpcRequest,
        response: mpsc::Sender<IpcResponse>,
    ) -> Option<IpcResponse> {
        match request {
            IpcRequest::Get { key } => {
                match get_config_key(&load_config(), &key).or_else(|| get_runtime_key(self, &key))
                {
                    Some(value) => Some(IpcResponse::Value { key, value }),
                    None => Some(IpcResponse::Error {
                        message: format!("unknown config key: {key}"),
                    }),
                }
            }
            IpcRequest::Set { key, value } => Some(self.set_config_key_and_notify(key, value)),
            IpcRequest::Watch { keys } => {
                if keys.is_empty() {
                    return Some(IpcResponse::Error {
                        message: "watch requires at least one key".to_string(),
                    });
                }

                let config = load_config();
                if let Some(key) = keys
                    .iter()
                    .find(|key| {
                        get_config_key(&config, key).is_none()
                            && get_runtime_key(self, key).is_none()
                    })
                    .cloned()
                {
                    return Some(IpcResponse::Error {
                        message: format!("unknown config key: {key}"),
                    });
                }

                self.settings_ipc_watchers
                    .push(DesktopIpcWatcher { keys, response });
                Some(IpcResponse::Ok)
            }
            IpcRequest::GetConfig => Some(IpcResponse::Config {
                config: load_config(),
            }),
            IpcRequest::SetConfig { config } => {
                let old_config = self.settings_ipc_config.clone();
                match save_config(&config) {
                    Ok(()) => {
                        self.notify_config_changes(&old_config, &config);
                        self.settings_ipc_config = config.clone();
                        self.apply_config(config);
                        Some(IpcResponse::Ok)
                    }
                    Err(err) => Some(IpcResponse::Error {
                        message: err.to_string(),
                    }),
                }
            }
            IpcRequest::ReloadConfig => {
                let old_config = self.settings_ipc_config.clone();
                let config = load_config();
                self.notify_config_changes(&old_config, &config);
                self.settings_ipc_config = config.clone();
                self.apply_config(config);
                Some(IpcResponse::Ok)
            }
            IpcRequest::Reload => {
                let old_config = self.settings_ipc_config.clone();
                let config = load_config();
                self.notify_config_changes(&old_config, &config);
                self.settings_ipc_config = config.clone();
                self.apply_config(config);

                let settings = load_settings();
                self.apps = settings.apps;
                self.privacy = settings.privacy;
                self.apply_debug_settings(settings.debug);
                self.mark_all_outputs_full_damage(DamageSource::Unknown);
                Some(IpcResponse::Ok)
            }
            IpcRequest::IdentifyDisplays => {
                self.topbar_pulse = Some(TopbarPulse {
                    output_id: self.focused_output,
                    indicator: 0,
                    click_local: (0.0, 0.0).into(),
                    started_at: Instant::now(),
                });
                self.mark_all_outputs_full_damage(DamageSource::Unknown);
                Some(IpcResponse::Ok)
            }
            IpcRequest::Notify {
                title,
                body,
                timeout_ms,
            } => {
                let timeout = timeout_ms.map(Duration::from_millis);
                let id = if timeout == Some(Duration::ZERO) {
                    self.notifications.push_persistent(title, body)
                } else if let Some(timeout) = timeout {
                    self.notifications
                        .push_with_timeout(title, body, Some(timeout))
                } else {
                    self.notifications.push(title, body)
                };
                self.mark_focused_output_full_damage(DamageSource::Unknown);
                Some(IpcResponse::Notification { id })
            }
            IpcRequest::AiPermissionPrompt {
                request_id,
                title,
                message,
                allow_persistent,
            } => {
                if self.active_dialog.is_some() || !self.pending_ai_permission_responses.is_empty()
                {
                    return Some(IpcResponse::Error {
                        message: "another modal dialog is already active".to_string(),
                    });
                }

                if self.outputs.is_empty() {
                    return Some(IpcResponse::Error {
                        message: "no output available for AI permission dialog".to_string(),
                    });
                }

                self.open_ai_permission_dialog(
                    request_id,
                    title,
                    message,
                    allow_persistent,
                    response,
                );
                None
            }
            IpcRequest::SetDisplays { outputs } => match self.apply_display_configs(outputs) {
                Ok(()) => Some(IpcResponse::Ok),
                Err(message) => Some(IpcResponse::Error { message }),
            },
            IpcRequest::GetDisplayRuntimeStatus => Some(IpcResponse::DisplayRuntimeStatus {
                outputs: self.runtime_display_statuses(),
            }),
            IpcRequest::GetAll | IpcRequest::SetValue { .. } => Some(IpcResponse::Error {
                message: "legacy settings.json IPC is not handled by focaldesk-desktop".to_string(),
            }),
        }
    }

    fn apply_debug_settings(&mut self, debug: DebugSettings) {
        apply_debug_log_level(debug.log_level);
        self.damage_debug_enabled = debug_damage_enabled(&debug);
        if debug.verbose_protocol_logs && !self.debug.verbose_protocol_logs {
            flog_info!(
                "verbose protocol logs are enabled for components that support runtime logging"
            );
        }
        self.debug = debug;
    }

    fn apply_display_configs(&mut self, outputs: Vec<OutputConfig>) -> Result<(), String> {
        let single_output_id = (self.outputs.len() == 1 && outputs.len() == 1)
            .then(|| self.outputs.keys().copied().next())
            .flatten();
        let mut changed = false;
        let mut unmatched = Vec::new();

        for config in outputs {
            let output_id = self
                .outputs
                .iter()
                .find_map(|(id, output)| (output.handle.name() == config.connector).then_some(*id))
                .or(single_output_id);

            let Some(output_id) = output_id else {
                unmatched.push(config.connector);
                continue;
            };

            let scale_factor = f64::from(config.scale);
            if !scale_factor.is_finite() || scale_factor < 1.0 {
                return Err(format!(
                    "invalid scale for {}: {}",
                    config.connector, config.scale
                ));
            }

            let physical_size = Size::<i32, Physical>::from((config.width, config.height));
            let logical_origin = Point::<i32, Logical>::from((config.x, config.y));

            if physical_size.w <= 0 || physical_size.h <= 0 {
                return Err(format!(
                    "invalid mode for {}: {}x{}",
                    config.connector, config.width, config.height
                ));
            }

            if let Some(output) = self.outputs.get_mut(&output_id) {
                output.logical_origin = logical_origin;
                let requested = config.hdr_requested || config.hdr_enabled;
                output.hdr_requested = output.hdr_supported && requested;
                if !output.hdr_requested {
                    output.hdr_enabled = false;
                }
            }
            self.update_output_size(output_id, physical_size, scale_factor);

            if config.primary {
                self.primary_output = output_id;
            }

            changed = true;
            flog_info!(
                "applied display IPC update name={} output={:?} size={}x{} scale={} origin={},{} primary={}",
                config.connector,
                output_id,
                config.width,
                config.height,
                scale_factor,
                config.x,
                config.y,
                config.primary
            );
        }

        if changed {
            self.mark_all_outputs_full_damage(DamageSource::Unknown);
            self.cursor_manager.set_base_size_and_scale(
                24,
                self.outputs
                    .get(&self.focused_output)
                    .or_else(|| self.outputs.get(&self.primary_output))
                    .map(|output| output.scale_factor as f32)
                    .unwrap_or(1.0),
            );
        }

        if !unmatched.is_empty() {
            flog_warn!(
                "display IPC update ignored unmatched outputs: {}",
                unmatched.join(", ")
            );
        }

        Ok(())
    }

    fn set_config_key_and_notify(&mut self, key: String, value: serde_json::Value) -> IpcResponse {
        let old_config = self.settings_ipc_config.clone();
        let mut config_value = match serde_json::to_value(&old_config) {
            Ok(value) => value,
            Err(err) => {
                return IpcResponse::Error {
                    message: err.to_string(),
                };
            }
        };

        if let Err(message) = set_json_key(&mut config_value, &key, value) {
            return IpcResponse::Error { message };
        }

        let new_config = match serde_json::from_value::<FocalDeskConfig>(config_value) {
            Ok(config) => config,
            Err(err) => {
                return IpcResponse::Error {
                    message: err.to_string(),
                };
            }
        };

        match save_config(&new_config) {
            Ok(()) => {
                self.notify_config_changes(&old_config, &new_config);
                self.settings_ipc_config = new_config.clone();
                self.apply_config(new_config);
                IpcResponse::Ok
            }
            Err(err) => IpcResponse::Error {
                message: err.to_string(),
            },
        }
    }

    fn notify_config_changes(
        &mut self,
        old_config: &FocalDeskConfig,
        new_config: &FocalDeskConfig,
    ) {
        let mut changed = Vec::new();

        for watcher in &self.settings_ipc_watchers {
            for key in &watcher.keys {
                if changed.iter().any(|(changed_key, _)| changed_key == key) {
                    continue;
                }

                let old_value = get_config_key(old_config, key);
                let new_value = get_config_key(new_config, key);
                if old_value != new_value {
                    if let Some(value) = new_value {
                        changed.push((key.clone(), value));
                    }
                }
            }
        }

        if changed.is_empty() {
            return;
        }

        self.settings_ipc_watchers.retain(|watcher| {
            for (key, value) in &changed {
                if watcher.keys.iter().any(|watched| watched == key)
                    && watcher
                        .response
                        .send(IpcResponse::Event {
                            key: key.clone(),
                            value: value.clone(),
                        })
                        .is_err()
                {
                    return false;
                }
            }

            true
        });
    }

    fn runtime_display_statuses(&self) -> Vec<DisplayRuntimeOutputStatus> {
        self.outputs
            .values()
            .map(|output| DisplayRuntimeOutputStatus {
                connector: output.handle.name().to_string(),
                icc_lut_fallback_active: output.icc_lut_fallback_active,
            })
            .collect()
    }

    pub(crate) fn notify_runtime_display_status_changes(&mut self) {
        let key = runtime_display_status_key().to_string();
        let value = runtime_display_status_value(self);

        self.settings_ipc_watchers.retain(|watcher| {
            if !watcher.keys.iter().any(|watched| watched == &key) {
                return true;
            }

            watcher
                .response
                .send(IpcResponse::Event {
                    key: key.clone(),
                    value: value.clone(),
                })
                .is_ok()
        });
    }

    fn apply_config(&mut self, config: FocalDeskConfig) {
        let old_theme_id = self.theme.active_theme().id.clone();
        let new_theme_id = theme_id_from_config(&config);

        self.theme.set_builtin(new_theme_id.clone());

        if old_theme_id != new_theme_id {
            if let Some(theme_id) = new_theme_id.builtin_id() {
                if let Err(err) = self.fonts.reload_for_theme(theme_id) {
                    flog_error!("failed to reload fonts for theme {:?}: {err}", theme_id);
                }
            }

            self.render.fonts_prewarm_done = false;
            self.render.font_atlas_texture = None;
        }

        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    /// Clears and returns whether the host (nested) window should begin a platform move drag.
    pub fn output_at_logical_point(&self, p: Point<f64, Logical>) -> Option<OutputId> {
        self.outputs
            .iter()
            .find(|(_, o)| {
                let x = p.x as i32;
                let y = p.y as i32;

                x >= o.logical_origin.x
                    && y >= o.logical_origin.y
                    && x < o.logical_origin.x + o.logical_size.w
                    && y < o.logical_origin.y + o.logical_size.h
            })
            .map(|(id, _)| *id)
    }
    pub fn update_ui_hover_for_output(&mut self, output_id: OutputId) -> bool {
        self.update_ui_hover_for_output_inner(output_id, true)
    }

    pub fn refresh_ui_hover_for_output(&mut self, output_id: OutputId) -> bool {
        self.update_ui_hover_for_output_inner(output_id, false)
    }

    fn update_ui_hover_for_output_inner(
        &mut self,
        output_id: OutputId,
        play_hover_sound: bool,
    ) -> bool {
        let old_hovered = self.ui.hovered;
        self.rebuild_ui_tree_for_output(output_id);
        if !self.output_contains_pointer(output_id) {
            self.ui.hovered = None;

            for el in &mut self.ui.elements {
                el.hovered = false;
            }

            return false;
        }

        let Some(rel) = self.pointer_relative_to_output_logical(output_id) else {
            return false;
        };
        let x = rel.x.round() as i32;
        let y = rel.y.round() as i32;

        let new_hovered = self.ui.hit_test(x, y).map(|e| e.id);
        self.ui.hovered = new_hovered;

        for el in &mut self.ui.elements {
            el.hovered = Some(el.id) == self.ui.hovered;
        }

        let sidebar_hover_sound_target = new_hovered
            .and_then(|id| self.ui.elements.iter().find(|el| el.id == id))
            .filter(|el| el.kind == UiElementKind::SidebarButton)
            .map(|el| (output_id, el.id));
        if play_hover_sound {
            if let Some(target) = sidebar_hover_sound_target {
                if self.last_sidebar_hover_sound_target != Some(target) {
                    self.play_ui_sound(UiSound::Hover);
                }
                self.last_sidebar_hover_sound_target = Some(target);
            } else {
                self.last_sidebar_hover_sound_target = None;
            }
        }

        if old_hovered == new_hovered {
            return false;
        }

        let mut damage = Vec::new();
        for id in [old_hovered, new_hovered].into_iter().flatten() {
            if let Some(el) = self.ui.elements.iter().find(|el| el.id == id) {
                damage.push(Rectangle::<i32, Logical>::from_loc_and_size(
                    (el.bounds.x, el.bounds.y),
                    (el.bounds.w, el.bounds.h),
                ));
                if el.tooltip.is_some() {
                    let tooltip_rect = Rectangle::<i32, Logical>::from_loc_and_size(
                        (el.bounds.x + el.bounds.w + 8, el.bounds.y - 2),
                        (240, el.bounds.h + 4),
                    );
                    damage.push(tooltip_rect);
                }
            }
        }

        for rect in damage {
            self.mark_output_logical_damage(output_id, rect, 10, DamageSource::Hover);
        }

        true
    }

    /// Compositor chrome hit (sidebar/topbar UI), if any, without consuming the event.
    pub(crate) fn peek_ui_action_at_pointer(&self) -> Option<focaldesk_ui::types::UiAction> {
        self.ui_element_at_pointer_for_output(self.focused_output)
            .filter(|el| el.enabled)
            .and_then(|el| el.action.clone())
    }

    pub(crate) fn ai_flow_mode(&self) -> AiFlowMode {
        if self.pending_ai_permission_responses.is_empty() {
            self.ai_flow_mode_cache
        } else {
            AiFlowMode::PermissionWait
        }
    }

    fn refresh_ai_flow_mode(&mut self) {
        if !self.pending_ai_permission_responses.is_empty() {
            self.ai_flow_mode_cache = AiFlowMode::PermissionWait;
            return;
        }

        let now = Instant::now();
        if now.saturating_duration_since(self.ai_flow_mode_last_poll) < Duration::from_millis(800) {
            return;
        }
        self.ai_flow_mode_last_poll = now;

        self.ai_flow_mode_cache = match send_ai_request(&AiIpcRequest::Status) {
            Ok(AiIpcResponse::Status { status }) => ai_flow_mode_from_status(&status),
            Ok(AiIpcResponse::Error { .. }) => AiFlowMode::Error,
            Ok(_) => AiFlowMode::Error,
            Err(_) => AiFlowMode::Error,
        };
    }

    fn ui_element_at_pointer_for_output(&self, output_id: OutputId) -> Option<&UiElement> {
        let local = self.pointer_relative_to_output_logical(output_id)?;
        let x = local.x.round() as i32;
        let y = local.y.round() as i32;
        self.ui.hit_test(x, y)
    }

    fn rebuild_ui_tree_for_output(&mut self, output_id: OutputId) {
        let Some(output) = self.outputs.get(&output_id) else {
            return;
        };
        let layout = build_chrome_layout(
            output.logical_size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        let ai_flow_mode = self.ai_flow_mode();
        build_ui_for_output_with_options(
            &mut self.ui,
            &layout,
            UiBuildOptions {
                hdr_supported: output.hdr_supported,
                hdr_enabled: output.hdr_enabled,
                workspace_count: self.workspace_names.len(),
                active_workspace: output.active_workspace.0,
                ai_flow_mode,
            },
        );
    }

    fn play_ui_sound(&self, sound: UiSound) {
        let buffer = self.render.resources.ui_sounds.get(sound);
        self.ui_sound_player.play(buffer);
    }

    pub fn click_ui_at_pointer(&mut self) -> bool {
        let Some(action) = self.peek_ui_action_at_pointer() else {
            return false;
        };
        flog(format!("ACTION={:?}", action));
        if self
            .ui_element_at_pointer_for_output(self.focused_output)
            .is_some_and(|el| el.kind == UiElementKind::TopbarFlowField)
        {
            let _ = self.trigger_flow_field_pulse_at_pointer(self.focused_output);
        }
        self.dispatch_ui_action(action);
        true
    }

    fn egui_modifiers(modifiers: FlowModifiers) -> EguiModifiers {
        EguiModifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: modifiers.super_key,
            command: modifiers.ctrl || modifiers.super_key,
        }
    }

    fn egui_frame_ctx_for_output(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<DesktopFrameCtx> {
        let output = self.outputs.get(&output_id)?;
        let layout = build_chrome_layout(
            output.logical_size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        Some(DesktopFrameCtx {
            output_size: (output.physical_size.w, output.physical_size.h),
            output_scale: output.scale,
            work: layout.work_area.recess,
            active_output: self.focused_output,
            rendering_output: output_id,
            now,
            start_time: self.render.start_time,
            flip_egui_y: self.backend_kind == BackendKind::Drm,
            portal_capture: false,
        })
    }

    pub fn sync_egui(&mut self, frame_ctx: &DesktopFrameCtx) {
        if !self.render.egui.has_open_panels() {
            self.render.egui.clear_paint();
            return;
        }
        self.render.egui.update_panels(frame_ctx);
        for action in self.render.egui.take_actions() {
            self.dispatch_ui_action(action);
        }
        if !self.render.egui.has_open_panels() {
            self.render.egui.clear_paint();
        }
    }

    fn process_egui_actions(&mut self) {
        if !self.render.egui.has_open_panels() {
            return;
        }
        let output_id = self
            .render
            .egui
            .owner_output()
            .unwrap_or(self.focused_output);
        let Some(frame_ctx) = self.egui_frame_ctx_for_output(output_id, Instant::now()) else {
            return;
        };
        self.sync_egui(&frame_ctx);
        self.mark_output_full_damage(output_id, DamageSource::Unknown);
    }

    fn egui_pointer_button(button: FlowMouseButton) -> Option<EguiPointerButton> {
        match button {
            FlowMouseButton::Left => Some(EguiPointerButton::Primary),
            FlowMouseButton::Right => Some(EguiPointerButton::Secondary),
            FlowMouseButton::Middle => Some(EguiPointerButton::Middle),
            FlowMouseButton::Back => Some(EguiPointerButton::Extra1),
            FlowMouseButton::Forward => Some(EguiPointerButton::Extra2),
            FlowMouseButton::Other(_) => None,
        }
    }

    fn egui_key(keycode: u32) -> Option<egui::Key> {
        let evdev_keycode = keycode.saturating_sub(8);
        match evdev_keycode {
            1 => Some(egui::Key::Escape),
            15 => Some(egui::Key::Tab),
            14 => Some(egui::Key::Backspace),
            28 | 96 => Some(egui::Key::Enter),
            57 => Some(egui::Key::Space),
            105 => Some(egui::Key::ArrowLeft),
            106 => Some(egui::Key::ArrowRight),
            103 => Some(egui::Key::ArrowUp),
            108 => Some(egui::Key::ArrowDown),
            102 => Some(egui::Key::Home),
            107 => Some(egui::Key::End),
            104 => Some(egui::Key::PageUp),
            109 => Some(egui::Key::PageDown),
            111 => Some(egui::Key::Delete),
            _ => None,
        }
    }

    fn egui_text_for_keycode(keycode: u32, modifiers: FlowModifiers) -> Option<String> {
        if modifiers.ctrl || modifiers.alt || modifiers.super_key {
            return None;
        }

        let shifted = modifiers.shift;
        let evdev_keycode = keycode.saturating_sub(8);
        let ch = match evdev_keycode {
            2 => {
                if shifted {
                    '!'
                } else {
                    '1'
                }
            }
            3 => {
                if shifted {
                    '@'
                } else {
                    '2'
                }
            }
            4 => {
                if shifted {
                    '#'
                } else {
                    '3'
                }
            }
            5 => {
                if shifted {
                    '$'
                } else {
                    '4'
                }
            }
            6 => {
                if shifted {
                    '%'
                } else {
                    '5'
                }
            }
            7 => {
                if shifted {
                    '^'
                } else {
                    '6'
                }
            }
            8 => {
                if shifted {
                    '&'
                } else {
                    '7'
                }
            }
            9 => {
                if shifted {
                    '*'
                } else {
                    '8'
                }
            }
            10 => {
                if shifted {
                    '('
                } else {
                    '9'
                }
            }
            11 => {
                if shifted {
                    ')'
                } else {
                    '0'
                }
            }
            12 => {
                if shifted {
                    '_'
                } else {
                    '-'
                }
            }
            13 => {
                if shifted {
                    '+'
                } else {
                    '='
                }
            }
            16 => {
                if shifted {
                    'Q'
                } else {
                    'q'
                }
            }
            17 => {
                if shifted {
                    'W'
                } else {
                    'w'
                }
            }
            18 => {
                if shifted {
                    'E'
                } else {
                    'e'
                }
            }
            19 => {
                if shifted {
                    'R'
                } else {
                    'r'
                }
            }
            20 => {
                if shifted {
                    'T'
                } else {
                    't'
                }
            }
            21 => {
                if shifted {
                    'Y'
                } else {
                    'y'
                }
            }
            22 => {
                if shifted {
                    'U'
                } else {
                    'u'
                }
            }
            23 => {
                if shifted {
                    'I'
                } else {
                    'i'
                }
            }
            24 => {
                if shifted {
                    'O'
                } else {
                    'o'
                }
            }
            25 => {
                if shifted {
                    'P'
                } else {
                    'p'
                }
            }
            26 => {
                if shifted {
                    '{'
                } else {
                    '['
                }
            }
            27 => {
                if shifted {
                    '}'
                } else {
                    ']'
                }
            }
            30 => {
                if shifted {
                    'A'
                } else {
                    'a'
                }
            }
            31 => {
                if shifted {
                    'S'
                } else {
                    's'
                }
            }
            32 => {
                if shifted {
                    'D'
                } else {
                    'd'
                }
            }
            33 => {
                if shifted {
                    'F'
                } else {
                    'f'
                }
            }
            34 => {
                if shifted {
                    'G'
                } else {
                    'g'
                }
            }
            35 => {
                if shifted {
                    'H'
                } else {
                    'h'
                }
            }
            36 => {
                if shifted {
                    'J'
                } else {
                    'j'
                }
            }
            37 => {
                if shifted {
                    'K'
                } else {
                    'k'
                }
            }
            38 => {
                if shifted {
                    'L'
                } else {
                    'l'
                }
            }
            39 => {
                if shifted {
                    ':'
                } else {
                    ';'
                }
            }
            40 => {
                if shifted {
                    '"'
                } else {
                    '\''
                }
            }
            41 => {
                if shifted {
                    '~'
                } else {
                    '`'
                }
            }
            43 => {
                if shifted {
                    '|'
                } else {
                    '\\'
                }
            }
            44 => {
                if shifted {
                    'Z'
                } else {
                    'z'
                }
            }
            45 => {
                if shifted {
                    'X'
                } else {
                    'x'
                }
            }
            46 => {
                if shifted {
                    'C'
                } else {
                    'c'
                }
            }
            47 => {
                if shifted {
                    'V'
                } else {
                    'v'
                }
            }
            48 => {
                if shifted {
                    'B'
                } else {
                    'b'
                }
            }
            49 => {
                if shifted {
                    'N'
                } else {
                    'n'
                }
            }
            50 => {
                if shifted {
                    'M'
                } else {
                    'm'
                }
            }
            51 => {
                if shifted {
                    '<'
                } else {
                    ','
                }
            }
            52 => {
                if shifted {
                    '>'
                } else {
                    '.'
                }
            }
            53 => {
                if shifted {
                    '?'
                } else {
                    '/'
                }
            }
            57 => ' ',
            _ => return None,
        };

        Some(ch.to_string())
    }

    fn handle_egui_input(&mut self, event: &FlowInputEvent) -> bool {
        if !self.render.egui.has_open_panels() {
            return false;
        }

        let Some(owner_output) = self.render.egui.owner_output() else {
            return false;
        };

        let egui_event = match *event {
            FlowInputEvent::PointerMoved { position } => {
                let Some(output_id) = self.output_under_pointer(position) else {
                    return false;
                };
                if output_id != owner_output {
                    return false;
                }
                let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
                    return false;
                };
                EguiInputEvent::PointerMoved { position: local }
            }
            FlowInputEvent::PointerButton {
                button,
                state,
                position,
                ..
            } => {
                let Some(button) = Self::egui_pointer_button(button) else {
                    return false;
                };
                let Some(output_id) = self.output_under_pointer(position) else {
                    return false;
                };
                if output_id != owner_output {
                    return false;
                }
                let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
                    return false;
                };
                EguiInputEvent::PointerButton {
                    button,
                    pressed: matches!(state, FlowKeyState::Pressed),
                    position: local,
                    modifiers: Self::egui_modifiers(self.input.modifiers),
                }
            }
            FlowInputEvent::PointerScroll {
                delta, position, ..
            } => {
                let Some(output_id) = self.output_under_pointer(position) else {
                    return false;
                };
                if output_id != owner_output {
                    return false;
                }
                let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
                    return false;
                };
                let delta = match delta {
                    FlowScrollDelta::Line { x, y } => EguiScrollDelta::Line { x, y },
                    FlowScrollDelta::Pixel { x, y } => EguiScrollDelta::Point {
                        x: x as f32,
                        y: y as f32,
                    },
                    FlowScrollDelta::Axis { x, y, .. } => EguiScrollDelta::Point {
                        x: x as f32,
                        y: y as f32,
                    },
                };
                EguiInputEvent::PointerScroll {
                    delta,
                    position: local,
                    modifiers: Self::egui_modifiers(self.input.modifiers),
                }
            }
            FlowInputEvent::PointerLeft => EguiInputEvent::PointerGone,
            FlowInputEvent::Key {
                keycode,
                state,
                repeat,
                modifiers,
            } => {
                if self.focused_output != owner_output {
                    return false;
                }
                EguiInputEvent::Key {
                    key: Self::egui_key(keycode),
                    text: Self::egui_text_for_keycode(keycode, modifiers),
                    pressed: matches!(state, FlowKeyState::Pressed),
                    repeat,
                    modifiers: Self::egui_modifiers(modifiers),
                }
            }
            _ => return false,
        };

        self.render.egui.handle_input(egui_event)
    }

    fn wl_pointer_time_ms() -> u32 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u32)
            .unwrap_or(0)
    }

    fn flow_mouse_to_evdev(button: FlowMouseButton) -> u32 {
        match button {
            FlowMouseButton::Left => 0x110,
            FlowMouseButton::Right => 0x111,
            FlowMouseButton::Middle => 0x112,
            FlowMouseButton::Back => 0x113,
            FlowMouseButton::Forward => 0x114,
            FlowMouseButton::Other(v) => v as u32,
        }
    }

    fn global_window_bbox(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        let space_bbox = self.space.element_bbox(window);
        let popup_bbox = self.space.element_location(window).map(|element_loc| {
            let mut bbox = window.bbox_with_popups();
            bbox.loc += element_loc - window.geometry().loc;
            bbox
        });

        match (space_bbox, popup_bbox) {
            (Some(space_bbox), Some(popup_bbox)) => Some(space_bbox.merge(popup_bbox)),
            (Some(space_bbox), None) => Some(space_bbox),
            (None, Some(popup_bbox)) => Some(popup_bbox),
            (None, None) => None,
        }
    }

    fn expand_logical_rect(rect: Rectangle<i32, Logical>, margin: i32) -> Rectangle<i32, Logical> {
        Rectangle::from_loc_and_size(
            (rect.loc.x - margin, rect.loc.y - margin),
            (rect.size.w + margin * 2, rect.size.h + margin * 2),
        )
    }

    fn mark_global_logical_damage(&mut self, rect: Rectangle<i32, Logical>) {
        const WINDOW_DAMAGE_MARGIN: i32 = 24;

        let rect = Self::expand_logical_rect(rect, WINDOW_DAMAGE_MARGIN);
        let mut damage = Vec::new();

        for (output_id, output) in &self.outputs {
            let output_rect =
                Rectangle::from_loc_and_size(output.logical_origin, output.logical_size);
            let Some(clipped) = rect.intersection(output_rect) else {
                continue;
            };

            let local = Rectangle::<i32, Logical>::from_loc_and_size(
                (
                    clipped.loc.x - output.logical_origin.x,
                    clipped.loc.y - output.logical_origin.y,
                ),
                clipped.size,
            );
            let physical = local.to_physical_precise_round::<f64, i32>(output.scale);
            damage.push((*output_id, physical));
        }

        for (output_id, rect) in damage {
            self.mark_output_damage_source(output_id, rect, DamageSource::Unknown);
        }
    }

    fn mark_window_bbox_damage(&mut self, rect: Rectangle<i32, Logical>) {
        self.mark_global_logical_damage(rect);
    }

    fn mark_window_bbox_damage_source(
        &mut self,
        rect: Rectangle<i32, Logical>,
        source: DamageSource,
    ) {
        const WINDOW_DAMAGE_MARGIN: i32 = 24;

        let rect = Self::expand_logical_rect(rect, WINDOW_DAMAGE_MARGIN);
        let mut damage = Vec::new();

        for (output_id, output) in &self.outputs {
            let output_rect =
                Rectangle::from_loc_and_size(output.logical_origin, output.logical_size);
            let Some(clipped) = rect.intersection(output_rect) else {
                continue;
            };

            let local = Rectangle::<i32, Logical>::from_loc_and_size(
                (
                    clipped.loc.x - output.logical_origin.x,
                    clipped.loc.y - output.logical_origin.y,
                ),
                clipped.size,
            );
            let physical = local.to_physical_precise_round::<f64, i32>(output.scale);
            damage.push((*output_id, physical));
        }

        for (output_id, rect) in damage {
            self.mark_output_damage_source(output_id, rect, source);
        }
    }

    pub(crate) fn mark_window_id_damage(&mut self, id: WindowId, source: DamageSource) {
        let Some(window) = self.window(id).map(|managed| managed.window.clone()) else {
            return;
        };
        if let Some(bbox) = self.global_window_bbox(&window) {
            self.mark_window_bbox_damage_source(bbox, source);
        }
    }

    pub(crate) fn mark_output_logical_damage(
        &mut self,
        output_id: OutputId,
        rect: Rectangle<i32, Logical>,
        margin: i32,
        source: DamageSource,
    ) {
        let Some(output) = self.outputs.get(&output_id) else {
            return;
        };

        let rect = Self::expand_logical_rect(rect, margin);
        let output_rect = Rectangle::<i32, Logical>::from_loc_and_size((0, 0), output.logical_size);
        let Some(clipped) = rect.intersection(output_rect) else {
            return;
        };

        let physical = clipped.to_physical_precise_round::<f64, i32>(output.scale);
        self.mark_output_damage_source(output_id, physical, source);
    }

    fn software_cursor_damage_pending_for_output(&self, output_id: OutputId) -> bool {
        self.output_owns_cursor(output_id)
            && self.cursor_manager.software_cursor_needed()
            && !self.drm_try_pass_cursor_this_frame
    }

    fn update_cursor_owner_damage(&mut self) -> bool {
        let owner = self
            .cursor_manager
            .visible()
            .then_some(self.focused_output)
            .filter(|&output_id| self.output_contains_pointer(output_id));

        if self.cursor_owner_output == owner {
            return false;
        }

        let old_owner = self.cursor_owner_output;
        self.cursor_owner_output = owner;

        if let Some(output_id) = old_owner {
            self.mark_output_full_damage(output_id, DamageSource::Cursor);
        }
        if let Some(output_id) = owner {
            self.mark_output_full_damage(output_id, DamageSource::Cursor);
        }

        true
    }

    fn clear_stale_software_cursor_damage(&mut self) -> bool {
        let pointer = self.pointer_pos;
        let stale: Vec<(OutputId, Rectangle<i32, Physical>)> = self
            .outputs
            .iter_mut()
            .filter_map(|(output_id, output)| {
                let owns_cursor = *output_id == self.focused_output
                    && pointer.x >= output.logical_origin.x as f64
                    && pointer.x < (output.logical_origin.x + output.logical_size.w) as f64
                    && pointer.y >= output.logical_origin.y as f64
                    && pointer.y < (output.logical_origin.y + output.logical_size.h) as f64;

                if owns_cursor {
                    return None;
                }

                output
                    .last_sw_cursor_rect
                    .take()
                    .map(|rect| (*output_id, Self::expand_physical_rect(rect, 4)))
            })
            .collect();

        let damaged = !stale.is_empty();
        for (output_id, rect) in stale {
            self.mark_output_damage_source(output_id, rect, DamageSource::Cursor);
        }
        damaged
    }

    fn clear_all_software_cursor_damage(&mut self) -> bool {
        let stale: Vec<(OutputId, Rectangle<i32, Physical>)> = self
            .outputs
            .iter_mut()
            .filter_map(|(output_id, output)| {
                output
                    .last_sw_cursor_rect
                    .take()
                    .map(|rect| (*output_id, Self::expand_physical_rect(rect, 4)))
            })
            .collect();

        let damaged = !stale.is_empty();
        for (output_id, rect) in stale {
            self.mark_output_damage_source(output_id, rect, DamageSource::Cursor);
        }
        damaged
    }

    pub(crate) fn map_window_bbox_location(
        &mut self,
        window: Window,
        bbox_loc: Point<i32, Logical>,
        activate: bool,
    ) {
        let space_loc = bbox_loc + window.geometry().loc;
        trace!(
            target: "focaldesk",
            ?bbox_loc,
            ?space_loc,
            activate,
            "map window bbox"
        );
        self.space.map_element(window, space_loc, activate);
    }

    /// Topmost client subsurface or xdg popup under `pos` (global logical), if any.
    pub(crate) fn pointer_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        let ws = self.focused_workspace();
        for window in self.space.elements() {
            window.on_commit();
        }

        if let Some((window, render_loc)) = self.space.element_under(pos) {
            let on_ws = self
                .windows
                .iter()
                .any(|mw| mw.mapped && mw.workspace == ws && &mw.window == window);
            if on_ws {
                #[cfg(feature = "xwayland")]
                if let Some(x11) = window.x11_surface() {
                    if let Some((_, surf_loc)) =
                        window.surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
                    {
                        return Some((
                            PointerFocusTarget::Xwayland(x11.clone()),
                            (surf_loc + render_loc).to_f64(),
                        ));
                    }
                }

                if let Some(hit) = window
                    .surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, surf_loc)| {
                        (
                            PointerFocusTarget::Wayland(surface),
                            (surf_loc + render_loc).to_f64(),
                        )
                    })
                {
                    return Some(hit);
                }
            }
        }

        // XWayland/GTK can briefly have empty input regions while geometry catches up;
        // fall back to bbox + surface_under (matches render visibility).
        for window in self.space.elements().rev() {
            let on_ws = self
                .windows
                .iter()
                .any(|mw| mw.mapped && mw.workspace == ws && &mw.window == window);
            if !on_ws {
                continue;
            }
            let Some(loc) = self.space.element_location(window) else {
                continue;
            };
            let render_loc = loc - window.geometry().loc;
            let Some(global) = self.global_window_bbox(window) else {
                continue;
            };
            if !global.to_f64().contains(pos) {
                continue;
            }
            #[cfg(feature = "xwayland")]
            if let Some(x11) = window.x11_surface() {
                if let Some((_, surf_loc)) =
                    window.surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
                {
                    return Some((
                        PointerFocusTarget::Xwayland(x11.clone()),
                        (surf_loc + render_loc).to_f64(),
                    ));
                }
            }
            if let Some((surface, surf_loc)) =
                window.surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
            {
                return Some((
                    PointerFocusTarget::Wayland(surface),
                    (surf_loc + render_loc).to_f64(),
                ));
            }
        }

        None
    }

    fn clear_client_pointer_focus(&mut self, pos: Point<f64, Logical>) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: pos,
                serial,
                time: Self::wl_pointer_time_ms(),
            },
        );
        pointer.frame(self);
    }

    fn compositor_pointer_grab_active(&self) -> bool {
        self.toplevel_pointer.is_some()
    }

    /// True when the seat pointer has an active click grab matching `serial` on `surface`.
    pub(crate) fn xdg_toplevel_pointer_grab_valid(
        &self,
        surface: &WlSurface,
        serial: Serial,
    ) -> bool {
        use wayland_server::Resource;

        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        if !pointer.has_grab(serial) {
            return false;
        }
        let Some(start_data) = pointer.grab_start_data() else {
            return false;
        };
        let Some((focus, _)) = start_data.focus.as_ref() else {
            return false;
        };
        focus
            .wl_surface()
            .map(|focus| focus.id().same_client_as(&surface.id()))
            .unwrap_or(false)
    }

    /// Deliver pointer motion to Wayland clients (nested compositor path).
    pub fn forward_pointer_to_clients(&mut self, pos: Point<f64, Logical>) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let under = self.pointer_surface_under(pos);
        static POINTER_FORWARD_LOGS: AtomicUsize = AtomicUsize::new(0);
        let seq = POINTER_FORWARD_LOGS.fetch_add(1, Ordering::Relaxed);
        if seq < 120 {
            flog(format!(
                "Pointer forward pos={:?} target={:?}",
                pos,
                under
                    .as_ref()
                    .map(|(target, surface_loc)| (target, surface_loc))
            ));
        }
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: Self::wl_pointer_time_ms(),
            },
        );
        pointer.frame(self);
    }

    fn forward_pointer_button(
        &mut self,
        _pos: Point<f64, Logical>,
        button: FlowMouseButton,
        state: FlowKeyState,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let smithay_state = match state {
            FlowKeyState::Pressed => ButtonState::Pressed,
            FlowKeyState::Released => ButtonState::Released,
        };
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time: Self::wl_pointer_time_ms(),
                button: Self::flow_mouse_to_evdev(button),
                state: smithay_state,
            },
        );
        pointer.frame(self);
    }

    fn forward_pointer_scroll(&mut self, delta: FlowScrollDelta) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = Self::wl_pointer_time_ms();
        let mut frame = match delta {
            FlowScrollDelta::Line { .. } => AxisFrame::new(time).source(AxisSource::Wheel),
            FlowScrollDelta::Pixel { .. } => AxisFrame::new(time).source(AxisSource::Wheel),
            FlowScrollDelta::Axis { source, .. } => {
                let source = match source {
                    FlowScrollSource::Finger => AxisSource::Finger,
                    FlowScrollSource::Continuous => AxisSource::Continuous,
                    FlowScrollSource::Wheel => AxisSource::Wheel,
                    FlowScrollSource::WheelTilt => AxisSource::WheelTilt,
                };
                AxisFrame::new(time).source(source)
            }
        };
        match delta {
            FlowScrollDelta::Line { x, y } => {
                if x != 0.0 {
                    frame = frame.value(Axis::Horizontal, f64::from(x));
                }
                if y != 0.0 {
                    frame = frame.value(Axis::Vertical, f64::from(y));
                }
            }
            FlowScrollDelta::Pixel { x, y } => {
                if x != 0.0 {
                    frame = frame.value(Axis::Horizontal, x);
                }
                if y != 0.0 {
                    frame = frame.value(Axis::Vertical, y);
                }
            }
            FlowScrollDelta::Axis {
                x,
                y,
                x_v120,
                y_v120,
                x_inverted,
                y_inverted,
                stop_x,
                stop_y,
                ..
            } => {
                if x != 0.0 {
                    if x_inverted {
                        frame = frame
                            .relative_direction(Axis::Horizontal, AxisRelativeDirection::Inverted);
                    }
                    frame = frame.value(Axis::Horizontal, x);
                    if let Some(v120) = x_v120 {
                        frame = frame.v120(Axis::Horizontal, v120);
                    }
                }
                if y != 0.0 {
                    if y_inverted {
                        frame = frame
                            .relative_direction(Axis::Vertical, AxisRelativeDirection::Inverted);
                    }
                    frame = frame.value(Axis::Vertical, y);
                    if let Some(v120) = y_v120 {
                        frame = frame.v120(Axis::Vertical, v120);
                    }
                }
                if stop_x {
                    frame = frame.stop(Axis::Horizontal);
                }
                if stop_y {
                    frame = frame.stop(Axis::Vertical);
                }
            }
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn resolve_launch_command(&self, cmd: &str) -> String {
        match cmd {
            "weston-terminal" | "@terminal" => self.apps.terminal.clone(),
            "google-chrome" | "@browser" => self.apps.browser.clone(),
            "nautilus" | "@files" => focaldesk_files_command(),
            "@settings" | "focaldesk-settings" => focaldesk_settings_command(),
            other => other.to_string(),
        }
    }

    pub(crate) fn dispatch_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::LaunchApp(cmd) => {
                self.launch_app(self.resolve_launch_command(cmd));
            }

            UiAction::OpenPanel(panel) => {
                self.render.egui.open_panel(panel, self.focused_output);
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }

            UiAction::CreateWorkspace(name) => {
                self.create_workspace_from_dialog(name);
            }

            UiAction::DeleteWorkspace => {
                self.delete_focused_workspace();
            }

            UiAction::SetSetting(setting, enabled) => {
                self.set_system_setting(setting, enabled);
            }

            UiAction::SetVolume(volume) => {
                self.set_default_audio_volume(volume);
            }

            UiAction::Custom(id) => match id {
                SIDEBAR_SETTINGS_ID => self.launch_app(focaldesk_settings_command()),
                focaldesk_ui::ui_builder::TOPBAR_FLOW_FIELD_ID => {
                    self.launch_app(focaldesk_ai_console_command())
                }
                SIDEBAR_WORKSPACE_1_ID => self.set_focused_workspace(WorkspaceId(1)),
                SIDEBAR_WORKSPACE_2_ID => {
                    if self.workspace_names.len() > 1 {
                        self.set_focused_workspace(WorkspaceId(2));
                    }
                }
                SIDEBAR_WORKSPACE_3_ID => {
                    if self.workspace_names.len() > 2 {
                        self.set_focused_workspace(WorkspaceId(3));
                    }
                }
                SIDEBAR_ADD_WORKSPACE_ID => {
                    let name = format!("Workspace {}", self.workspace_names.len() + 1);
                    self.render
                        .egui
                        .open_add_workspace_dialog(self.focused_output, name);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                }
                SIDEBAR_DELETE_WORKSPACE_ID => {
                    if self.workspace_names.len() > 1 {
                        self.render
                            .egui
                            .open_delete_workspace_dialog(self.focused_output);
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                    }
                }
                SIDEBAR_BROWSER_ID => self.launch_app(self.apps.browser.clone()),
                SIDEBAR_TERMINAL_ID => self.launch_app(self.apps.terminal.clone()),
                SIDEBAR_FILES_ID => self.launch_app(focaldesk_files_command()),
                _ => flog_warn!("unhandled custom ui action: {id}"),
            },

            UiAction::SystemCommand(cmd) => {
                self.dispatch_system_command(cmd);
            }

            UiAction::ToggleSetting(setting) => {
                flog_info!("TODO toggle setting: {:?}", setting);
            }
        }
    }

    fn set_system_setting(&self, setting: focaldesk_ui::types::SettingKey, enabled: bool) {
        match setting {
            focaldesk_ui::types::SettingKey::Wifi => {
                let state = if enabled { "on" } else { "off" };
                if let Err(err) = Command::new("nmcli").args(["radio", "wifi", state]).spawn() {
                    flog_error!("failed to set wifi radio {state}: {err}");
                }
            }
            focaldesk_ui::types::SettingKey::Bluetooth => {
                let state = if enabled { "on" } else { "off" };
                if let Err(err) = Command::new("bluetoothctl").args(["power", state]).spawn() {
                    flog_error!("failed to set bluetooth power {state}: {err}");
                }
            }
            focaldesk_ui::types::SettingKey::DoNotDisturb => {
                flog_warn!("do-not-disturb setting is not implemented");
            }
        }
    }

    fn set_default_audio_volume(&self, volume: f32) {
        let percent = (volume.clamp(0.0, 1.0) * 100.0).round();
        if let Err(err) = Command::new("wpctl")
            .args([
                "set-volume",
                "@DEFAULT_AUDIO_SINK@",
                &format!("{percent:.0}%"),
            ])
            .spawn()
        {
            flog_error!("failed to set default audio volume: {err}");
        }
    }

    fn dispatch_system_command(&mut self, cmd: focaldesk_ui::types::SystemCommand) {
        match cmd {
            focaldesk_ui::types::SystemCommand::Lock => {
                self.lock_session();
            }
            focaldesk_ui::types::SystemCommand::Logout => {
                self.running = false;
            }
            focaldesk_ui::types::SystemCommand::Restart => {
                if let Err(err) = focaldesk_power::PowerManager::new().reboot() {
                    flog_error!("failed to start reboot: {err}");
                }
            }
            focaldesk_ui::types::SystemCommand::Shutdown => {
                if let Err(err) = focaldesk_power::PowerManager::new().power_off() {
                    flog_error!("failed to start poweroff: {err}");
                }
            }
        }
    }

    fn lock_session(&mut self) {
        self.render.egui.close_all_panels();
        self.active_dialog = None;
        self.clear_client_pointer_focus(self.pointer_pos);
        self.lock_screen.lock();
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    fn submit_lock_password(&mut self) {
        if !self.lock_screen.active || self.lock_screen.authenticating {
            return;
        }

        if self.lock_screen.password.is_empty() {
            self.lock_screen.message = "Enter password".to_string();
            self.lock_screen.pulse(LockPulseKind::Rejected);
            self.mark_all_outputs_full_damage(DamageSource::Unknown);
            return;
        }

        self.lock_screen.authenticating = true;
        self.lock_screen.message = "Authenticating".to_string();
        self.mark_all_outputs_full_damage(DamageSource::Unknown);

        let password = self.lock_screen.password.clone();
        let authenticated = match authenticate_current_user(&password) {
            Ok(authenticated) => authenticated,
            Err(err) => {
                flog_error!("PAM authentication failed: {err}");
                false
            }
        };

        self.lock_screen.authenticating = false;
        self.lock_screen.clear_password();

        if authenticated {
            self.lock_screen.message = "Unlocked".to_string();
            self.lock_screen.pulse(LockPulseKind::Accepted);
        } else {
            self.lock_screen.message = "Wrong password".to_string();
            self.lock_screen.pulse(LockPulseKind::Rejected);
        }

        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    fn toggle_lock_password_visibility_at(&mut self, position: Point<f64, Logical>) -> bool {
        let Some((output_id, local)) = self.output_local_point(position) else {
            return false;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let logical_size = output.logical_size;
        let panel_w = logical_size.w.min(460).max(320);
        let panel_h = 190;
        let panel_x = (logical_size.w - panel_w) / 2;
        let panel_y = (logical_size.h - panel_h) / 2;
        let field = Rectangle::<i32, Logical>::from_loc_and_size(
            (panel_x + 28, panel_y + 72),
            (panel_w - 56, 48),
        );
        let reveal_button = Rectangle::<i32, Logical>::from_loc_and_size(
            (field.loc.x + field.size.w - 82, field.loc.y + 8),
            (68, 32),
        );

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;
        if reveal_button.contains((px, py)) {
            self.lock_screen.toggle_password_visibility();
            self.mark_all_outputs_full_damage(DamageSource::Unknown);
            true
        } else {
            false
        }
    }
    //}

    fn output_local_point(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(OutputId, Point<f64, Logical>)> {
        let output_id = self.output_under_pointer(position)?;
        let output = self.outputs.get(&output_id)?;

        let local = Point::from((
            position.x - output.logical_origin.x as f64,
            position.y - output.logical_origin.y as f64,
        ));

        Some((output_id, local))
    }

    pub fn request_screenshot(&mut self) {
        let output_id = self
            .output_under_pointer(self.pointer_pos)
            .unwrap_or(self.focused_output);
        self.screenshot_requested = Some(output_id);
        self.mark_output_full_damage(output_id, DamageSource::Unknown);
        dbg_flush("SCREENSHOT REQUEST SET");
    }
    pub fn request_screenshot_all(&mut self) {
        self.screenshot_all_requested = true;
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
        dbg_flush("SCREENSHOT ALL REQUEST SET");
    }

    pub fn take_screenshot_request(&mut self) -> Option<OutputId> {
        self.screenshot_requested.take()
    }

    pub fn screenshot_request(&self) -> Option<OutputId> {
        self.screenshot_requested
    }

    pub fn clear_screenshot_request(&mut self, output_id: OutputId) {
        if self.screenshot_requested == Some(output_id) {
            self.screenshot_requested = None;
        }
    }

    pub fn workspace_under_pointer(&self, pos: Point<f64, Logical>) -> WorkspaceId {
        self.output_under_pointer(pos)
            .and_then(|id| self.outputs.get(&id))
            .map(|o| o.active_workspace)
            .unwrap_or_else(|| self.focused_workspace())
    }

    pub fn focused_output_state(&self) -> Option<&OutputState> {
        self.outputs.get(&self.focused_output)
    }

    fn set_focused_output(&mut self, output_id: OutputId) {
        if self.focused_output == output_id {
            return;
        }

        let previous = self.focused_output;
        self.focused_output = output_id;
        self.focus_changed_at = Instant::now();
        self.mark_output_full_damage(previous, DamageSource::Unknown);
        self.mark_output_full_damage(output_id, DamageSource::Unknown);
    }

    pub fn focused_workspace(&self) -> WorkspaceId {
        self.outputs
            .get(&self.focused_output)
            .map(|o| o.active_workspace)
            .unwrap_or(self.active_workspace)
    }

    pub fn set_focused_workspace(&mut self, workspace: WorkspaceId) {
        if workspace.0 == 0 || workspace.0 as usize > self.workspace_names.len() {
            return;
        }
        if let Some(output) = self.outputs.get_mut(&self.focused_output) {
            output.active_workspace = workspace;
        }
        self.active_workspace = workspace;
        self.rebuild_ui_tree_for_output(self.focused_output);
        self.mark_focused_output_full_damage(DamageSource::Unknown);
    }

    fn create_workspace_from_dialog(&mut self, name: String) {
        let next_number = self.workspace_names.len() + 1;
        let name = if name.trim().is_empty() {
            format!("Workspace {next_number}")
        } else {
            name.trim().to_string()
        };

        self.workspace_names.push(name);
        self.set_focused_workspace(WorkspaceId(next_number as u32));
        self.mark_all_outputs_chrome_controls_damage(DamageSource::Unknown);
    }

    fn delete_focused_workspace(&mut self) {
        if self.workspace_names.len() <= 1 {
            return;
        }

        let current = self.focused_workspace().0.max(1) as usize;
        let delete_index = current.min(self.workspace_names.len()) - 1;
        self.workspace_names.remove(delete_index);

        let next_workspace = delete_index.min(self.workspace_names.len().saturating_sub(1)) + 1;
        self.set_focused_workspace(WorkspaceId(next_workspace as u32));
        self.mark_all_outputs_chrome_controls_damage(DamageSource::Unknown);
    }

    pub fn register_output_entry(
        &mut self,
        output_id: OutputId,
        handle: Output,
        logical_origin: Point<i32, Logical>,
        physical_size: Size<i32, Physical>,
        scale_factor: f64,
    ) {
        let logical_w = ((physical_size.w as f64) / scale_factor).round() as i32;
        let logical_h = ((physical_size.h as f64) / scale_factor).round() as i32;
        let logical_size = Size::<i32, Logical>::from((logical_w, logical_h));

        self.space.map_output(&handle, logical_origin);

        //let logical_origin = if self.outputs.is_empty() {
        //    Point::<i32, Logical>::from((0, 0))
        //} else {
        //    let x = self.outputs.values()
        //    .map(|o| o.logical_origin.x + o.logical_size.w)
        //    .max()
        //    .unwrap_or(0);
        //    Point::<i32, Logical>::from((x, 0))
        //};

        let entry = self.outputs.entry(output_id).or_insert_with(|| {
            // Replace this with your real output-state struct constructor/default.
            crate::core::desktop::OutputState {
                handle: handle.clone(),
                physical_size,
                logical_size,
                logical_origin,
                scale_factor,
                scale: Scale::from((scale_factor, scale_factor)),
                hdr_supported: false,
                hdr_requested: false,
                hdr_enabled: false,
                edid_hdr_max_luminance_nits: None,
                edid_hdr_max_fall_nits: None,
                active_workspace: WorkspaceId(1),
                pending_damage: vec![Rectangle::from_loc_and_size((0, 0), physical_size)],
                last_sw_cursor_rect: None,
                color_description: crate::core::color::default_output_color_description(),
                icc_profile: None,
                output_icc_lut: None,
                icc_lut_fallback_active: false,
                monitor_make: String::new(),
                monitor_model: String::new(),
                monitor_serial: String::new(),
                monitor_edid: None,
            }
        });

        entry.handle = handle;
        entry.physical_size = physical_size;
        entry.logical_size = logical_size;
        entry.logical_origin = logical_origin;
        entry.scale_factor = scale_factor;
        entry.scale = Scale::from((scale_factor, scale_factor));
        entry.pending_damage = vec![Rectangle::from_loc_and_size((0, 0), physical_size)];
        entry.last_sw_cursor_rect = None;

        // Optional: choose first registered output as active if needed
        if self.outputs.len() == 1 {
            self.primary_output = output_id;
        }
    }

    pub fn output_contains_pointer(&self, output_id: OutputId) -> bool {
        let pointer = self.pointer_pos; // logical coords

        if let Some(output) = self.outputs.get(&output_id) {
            let ox = output.logical_origin.x;
            let oy = output.logical_origin.y;
            let ow = output.logical_size.w;
            let oh = output.logical_size.h;

            return pointer.x >= ox as f64
                && pointer.x < (ox + ow) as f64
                && pointer.y >= oy as f64
                && pointer.y < (oy + oh) as f64;
        }

        false
    }

    pub fn output_owns_cursor(&self, output_id: OutputId) -> bool {
        output_id == self.focused_output && self.output_contains_pointer(output_id)
    }

    /// Bounding rectangle of all outputs in global logical space (for clamping pointer motion).
    pub fn logical_pointer_clamp_rect(&self) -> Rectangle<i32, Logical> {
        let mut it = self.outputs.values();
        let Some(first) = it.next() else {
            return Rectangle::from_loc_and_size((0, 0), (8192, 8192));
        };
        let mut min_x = first.logical_origin.x;
        let mut min_y = first.logical_origin.y;
        let mut max_x = first.logical_origin.x + first.logical_size.w;
        let mut max_y = first.logical_origin.y + first.logical_size.h;
        for o in it {
            min_x = min_x.min(o.logical_origin.x);
            min_y = min_y.min(o.logical_origin.y);
            max_x = max_x.max(o.logical_origin.x + o.logical_size.w);
            max_y = max_y.max(o.logical_origin.y + o.logical_size.h);
        }
        Rectangle::from_loc_and_size((min_x, min_y), (max_x - min_x, max_y - min_y))
    }

    /// Which output the pointer lies in (first match in output map order), if any.
    pub fn output_under_pointer(&self, pointer: Point<f64, Logical>) -> Option<OutputId> {
        self.outputs.keys().copied().find(|&id| {
            self.outputs.get(&id).is_some_and(|output| {
                let ox = output.logical_origin.x;
                let oy = output.logical_origin.y;
                let ow = output.logical_size.w;
                let oh = output.logical_size.h;
                pointer.x >= ox as f64
                    && pointer.x < (ox + ow) as f64
                    && pointer.y >= oy as f64
                    && pointer.y < (oy + oh) as f64
            })
        })
    }

    /// Pointer position relative to an output's top-left (logical).
    pub fn pointer_relative_to_output_logical(
        &self,
        output_id: OutputId,
    ) -> Option<Point<f64, Logical>> {
        let o = self.outputs.get(&output_id)?;
        Some(Point::from((
            self.pointer_pos.x - f64::from(o.logical_origin.x),
            self.pointer_pos.y - f64::from(o.logical_origin.y),
        )))
    }

    pub fn take_host_window_drag_request(&mut self) -> bool {
        let v = self.host_window_drag_requested;
        self.host_window_drag_requested = false;
        v
    }

    pub(crate) fn suppress_next_left_release(&mut self) {
        self.suppress_next_left_release = true;
    }

    fn pointer_on_chrome_host_drag_region(&self, position: Point<f64, Logical>) -> bool {
        let Some((output_id, local)) = self.output_local_point(position) else {
            return false;
        };

        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;

        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));

        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );

        chrome_host_drag_hit(&layout, px, py)
    }

    /// Main content region below the top bar and right of the sidebar (wallpaper / client stack).
    fn pointer_in_work_recess(&self, position: Point<f64, Logical>) -> bool {
        let Some((output_id, local)) = self.output_local_point(position) else {
            return false;
        };

        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;

        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));

        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );

        layout.work_area.recess.contains((px, py))
    }

    pub(crate) fn work_recess_for_output(
        &self,
        output_id: OutputId,
    ) -> Option<Rectangle<i32, Logical>> {
        let output = self.outputs.get(&output_id)?;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        let mut recess = layout.work_area.recess;
        recess.loc += output.logical_origin;
        Some(recess)
    }

    #[cfg(feature = "xwayland")]
    fn output_logical_rect(&self, output_id: OutputId) -> Option<Rectangle<i32, Logical>> {
        let output = self.outputs.get(&output_id)?;
        Some(Rectangle::from_loc_and_size(
            output.logical_origin,
            Size::from((output.logical_size.w, output.logical_size.h)),
        ))
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_output_id_for_window(&self, id: WindowId) -> OutputId {
        self.window(id)
            .and_then(|window| window.output)
            .or_else(|| self.output_under_pointer(self.input.pointer_pos))
            .unwrap_or(self.focused_output)
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_request_fills_output(
        &self,
        output_id: OutputId,
        size: Size<i32, Logical>,
    ) -> bool {
        let Some(output_rect) = self.output_logical_rect(output_id) else {
            return false;
        };
        const SLACK: i32 = 16;
        size.w >= output_rect.size.w.saturating_sub(SLACK)
            && size.h >= output_rect.size.h.saturating_sub(SLACK)
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_compositor_loc_for_window(&self, id: WindowId) -> Point<i32, Logical> {
        let output_id = self.xwayland_output_id_for_window(id);
        let Some(window) = self.window(id) else {
            return self.default_toplevel_map_location(output_id);
        };
        self.space
            .element_bbox(&window.window)
            .map(|bbox| bbox.loc)
            .or(window.float_rect.map(|rect| rect.loc))
            .unwrap_or_else(|| self.default_toplevel_map_location(output_id))
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_clamp_toplevel_geometry(
        &self,
        output_id: OutputId,
        mut geometry: Rectangle<i32, Logical>,
        window_id: Option<WindowId>,
    ) -> Rectangle<i32, Logical> {
        let Some(work) = self.work_recess_for_output(output_id) else {
            return geometry;
        };
        if self.xwayland_request_fills_output(output_id, geometry.size) {
            return work;
        }
        geometry.loc = window_id
            .map(|id| self.xwayland_compositor_loc_for_window(id))
            .unwrap_or_else(|| self.default_toplevel_map_location(output_id));
        geometry.size.w = geometry.size.w.clamp(1, work.size.w);
        geometry.size.h = geometry.size.h.clamp(1, work.size.h);
        geometry
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_clamp_override_redirect_geometry(
        &self,
        output_id: OutputId,
        geometry: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let Some(bounds) = self
            .work_recess_for_output(output_id)
            .or_else(|| self.output_logical_rect(output_id))
        else {
            return geometry;
        };
        clamp_rect_to_bounds(geometry, bounds)
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn xwayland_or_compositor_loc(
        &self,
        surface: &smithay::xwayland::X11Surface,
        x11_loc: Point<i32, Logical>,
    ) -> Point<i32, Logical> {
        let Some(parent_xid) = surface.is_transient_for() else {
            return x11_loc;
        };
        let Some(parent) = self.windows.iter().find(|managed| {
            managed
                .window
                .x11_surface()
                .is_some_and(|x11| x11.window_id() == parent_xid)
        }) else {
            return x11_loc;
        };
        // Use the parent's geometry origin, not its bbox origin.
        // `element_bbox()` includes any already-mapped popups, which can shift
        // the anchor for new override-redirect children as menus open and close.
        let Some(compositor_parent_loc) =
            self.space.element_location(&parent.window).or_else(|| {
                parent
                    .float_rect
                    .map(|rect| rect.loc + parent.window.geometry().loc)
            })
        else {
            return x11_loc;
        };
        let x11_parent_loc = parent
            .window
            .x11_surface()
            .map(|x11| x11.geometry().loc)
            .unwrap_or(Point::from((0, 0)));
        x11_loc + (compositor_parent_loc - x11_parent_loc)
    }

    fn output_id_for_space_output(&self, output: &Output) -> Option<OutputId> {
        self.outputs
            .iter()
            .find_map(|(id, state)| (&state.handle == output).then_some(*id))
    }

    /// Default map location for a new toplevel: top-left of the work recess (global logical).
    fn default_toplevel_map_location(&self, output_id: OutputId) -> Point<i32, Logical> {
        self.work_recess_for_output(output_id)
            .map(|work| work.loc)
            .unwrap_or_else(|| {
                self.outputs
                    .get(&output_id)
                    .map(|out| {
                        Point::from((out.logical_origin.x + 100, out.logical_origin.y + 100))
                    })
                    .unwrap_or(Point::from((100, 100)))
            })
    }

    fn clamp_window_location_to_work_recess(
        &self,
        window: &Window,
        proposed_loc: Point<i32, Logical>,
        pointer_pos: Point<f64, Logical>,
    ) -> Point<i32, Logical> {
        let output_id = self
            .output_under_pointer(pointer_pos)
            .unwrap_or(self.focused_output);
        let Some(work) = self.work_recess_for_output(output_id) else {
            return proposed_loc;
        };

        let geometry = window.geometry();
        let bbox = window.bbox_with_popups();
        let render_offset = geometry.loc - bbox.loc;

        let min_x = work.loc.x + render_offset.x;
        let min_y = work.loc.y + render_offset.y;
        let max_x = work.loc.x + work.size.w - bbox.size.w + render_offset.x;
        let max_y = work.loc.y + work.size.h - bbox.size.h + render_offset.y;

        Point::from((
            proposed_loc.x.clamp(min_x.min(max_x), max_x.max(min_x)),
            proposed_loc.y.clamp(min_y.min(max_y), max_y.max(min_y)),
        ))
    }

    /// Hovered sidebar slot for this output only (global pointer + per-output chrome layout).
    pub fn sidebar_hover_for_output(&self, output_id: OutputId) -> Option<usize> {
        if !self.output_contains_pointer(output_id) {
            return None;
        }
        let output = self.outputs.get(&output_id)?;
        let px = self.pointer_pos.x.round() as i32 - output.logical_origin.x;
        let py = self.pointer_pos.y.round() as i32 - output.logical_origin.y;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        sidebar_slot_index_at(&layout, px, py)
    }

    pub fn sidebar_pulse_for_output(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<SidebarPulseFrame> {
        let pulse = self.sidebar_pulse?;
        if pulse.output_id != output_id {
            return None;
        }

        let elapsed = now.saturating_duration_since(pulse.started_at);
        if elapsed >= SIDEBAR_PULSE_DURATION {
            return None;
        }

        Some(SidebarPulseFrame {
            slot: pulse.slot,
            click_local: pulse.click_local,
            elapsed,
        })
    }

    pub fn output_has_active_sidebar_pulse(&self, output_id: OutputId, now: Instant) -> bool {
        self.sidebar_pulse_for_output(output_id, now).is_some()
    }

    pub fn topbar_pulse_for_output(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<TopbarPulseFrame> {
        let pulse = self.topbar_pulse?;
        if pulse.output_id != output_id {
            return None;
        }

        let elapsed = now.saturating_duration_since(pulse.started_at);
        if elapsed >= TOPBAR_PULSE_DURATION {
            return None;
        }

        Some(TopbarPulseFrame {
            indicator: pulse.indicator,
            click_local: pulse.click_local,
            elapsed,
        })
    }

    pub fn output_has_active_topbar_pulse(&self, output_id: OutputId, now: Instant) -> bool {
        self.topbar_pulse_for_output(output_id, now).is_some()
    }

    pub fn flow_field_pulse_for_output(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<FlowFieldPulseFrame> {
        let pulse = self.flow_field_pulse?;
        if pulse.output_id != output_id {
            return None;
        }

        let elapsed = now.saturating_duration_since(pulse.started_at);
        if elapsed >= FLOW_FIELD_PULSE_DURATION {
            return None;
        }

        Some(FlowFieldPulseFrame {
            click_local: pulse.click_local,
            elapsed,
        })
    }

    pub fn output_has_active_flow_field_pulse(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> bool {
        self.flow_field_pulse_for_output(output_id, now).is_some()
    }

    pub fn clock_pulse_for_output(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<ClockPulseFrame> {
        let pulse = self.clock_pulse?;
        if pulse.output_id != output_id {
            return None;
        }

        let elapsed = now.saturating_duration_since(pulse.started_at);
        if elapsed >= CLOCK_PULSE_DURATION {
            return None;
        }

        Some(ClockPulseFrame {
            click_local: pulse.click_local,
            elapsed,
        })
    }

    pub fn output_has_active_clock_pulse(&self, output_id: OutputId, now: Instant) -> bool {
        self.clock_pulse_for_output(output_id, now).is_some()
    }

    pub fn active_sidebar_pulse_damage_rect(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<Rectangle<i32, Logical>> {
        let pulse = self.sidebar_pulse_for_output(output_id, now)?;
        let output = self.outputs.get(&output_id)?;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        layout.sidebar.slots.get(pulse.slot).map(|slot| slot.outer)
    }

    pub fn active_topbar_pulse_damage_rect(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<Rectangle<i32, Logical>> {
        let pulse = self.topbar_pulse_for_output(output_id, now)?;
        let output = self.outputs.get(&output_id)?;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        layout.topbar.status_wells.get(pulse.indicator).copied()
    }

    pub fn active_flow_field_pulse_damage_rect(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<Rectangle<i32, Logical>> {
        self.flow_field_pulse_for_output(output_id, now)?;
        let output = self.outputs.get(&output_id)?;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        Some(layout.topbar.flow_field)
    }

    pub fn active_clock_pulse_damage_rect(
        &self,
        output_id: OutputId,
        now: Instant,
    ) -> Option<Rectangle<i32, Logical>> {
        self.clock_pulse_for_output(output_id, now)?;
        let output = self.outputs.get(&output_id)?;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        Some(layout.topbar.clock_well)
    }

    fn trigger_sidebar_pulse_at_pointer(&mut self, output_id: OutputId) -> bool {
        let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
            return false;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        let Some(slot) = sidebar_slot_index_at(&layout, px, py) else {
            return false;
        };

        self.sidebar_pulse = Some(SidebarPulse {
            output_id,
            slot,
            click_local: local,
            started_at: Instant::now(),
        });

        if let Some(slot_layout) = layout.sidebar.slots.get(slot) {
            self.mark_output_logical_damage(output_id, slot_layout.outer, 0, DamageSource::Unknown);
        }

        true
    }

    fn trigger_topbar_pulse_at_pointer(&mut self, output_id: OutputId) -> bool {
        let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
            return false;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        let Some(indicator) = topbar_status_well_index_at(&layout, px, py) else {
            return false;
        };

        self.topbar_pulse = Some(TopbarPulse {
            output_id,
            indicator,
            click_local: local,
            started_at: Instant::now(),
        });

        if let Some(rect) = layout.topbar.status_wells.get(indicator) {
            self.mark_output_logical_damage(output_id, *rect, 0, DamageSource::Unknown);
        }

        true
    }

    fn trigger_flow_field_pulse_at_pointer(&mut self, output_id: OutputId) -> bool {
        let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
            return false;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        if !layout.topbar.flow_field.contains((px, py)) {
            return false;
        }

        self.flow_field_pulse = Some(FlowFieldPulse {
            output_id,
            click_local: local,
            started_at: Instant::now(),
        });

        self.mark_output_logical_damage(
            output_id,
            layout.topbar.flow_field,
            0,
            DamageSource::Unknown,
        );

        true
    }

    fn trigger_clock_pulse_at_pointer(&mut self, output_id: OutputId) -> bool {
        let Some(local) = self.pointer_relative_to_output_logical(output_id) else {
            return false;
        };
        let Some(output) = self.outputs.get(&output_id) else {
            return false;
        };

        let px = local.x.round() as i32;
        let py = local.y.round() as i32;
        let size = Size::<i32, Logical>::from((output.logical_size.w, output.logical_size.h));
        let layout = build_chrome_layout(
            size,
            self.chrome.metrics.topbar_h,
            self.chrome.metrics.sidebar_w,
        );
        if !layout.topbar.clock_well.contains((px, py)) {
            return false;
        }

        self.clock_pulse = Some(ClockPulse {
            output_id,
            click_local: local,
            started_at: Instant::now(),
        });

        self.mark_output_logical_damage(
            output_id,
            layout.topbar.clock_well,
            0,
            DamageSource::Unknown,
        );

        true
    }

    fn top_mapped_window_id_at(&self, position: Point<f64, Logical>) -> Option<WindowId> {
        let px = position.x.round() as i32;
        let py = position.y.round() as i32;
        let ws = self.focused_workspace();
        self.space.elements().rev().find_map(|window| {
            let managed = self
                .windows
                .iter()
                .find(|mw| mw.mapped && mw.workspace == ws && &mw.window == window)?;
            self.global_window_bbox(window)
                .is_some_and(|bbox| bbox.contains((px, py)))
                .then_some(managed.id)
        })
    }

    fn xwayland_titlebar_window_id_at(&self, position: Point<f64, Logical>) -> Option<WindowId> {
        const TITLEBAR_H: i32 = 36;
        const RESIZE_EDGE_GUARD: i32 = RESIZE_BORDER_PX + 1;

        if !self.pointer_in_work_recess(position) {
            return None;
        }

        let px = position.x.round() as i32;
        let py = position.y.round() as i32;
        let ws = self.focused_workspace();

        self.space.elements().rev().find_map(|window| {
            let managed = self
                .windows
                .iter()
                .find(|mw| mw.mapped && mw.workspace == ws && &mw.window == window)?;
            if !managed.mapped
                || managed.fullscreen
                || managed.minimized
                || window.x11_surface().is_none()
            {
                return None;
            }

            let bbox = self.global_window_bbox(window)?;
            let titlebar = Rectangle::<i32, Logical>::from_loc_and_size(
                (bbox.loc.x, bbox.loc.y + RESIZE_EDGE_GUARD),
                (bbox.size.w, (TITLEBAR_H - RESIZE_EDGE_GUARD).max(1)),
            );

            titlebar.contains((px, py)).then_some(managed.id)
        })
    }

    fn handle_xwayland_titlebar_press(&mut self, position: Point<f64, Logical>) -> bool {
        const DOUBLE_CLICK_MAX: Duration = Duration::from_millis(500);
        const DOUBLE_CLICK_DISTANCE_SQ: f64 = 6.0 * 6.0;

        let Some(id) = self.xwayland_titlebar_window_id_at(position) else {
            self.last_titlebar_click = None;
            return false;
        };

        let now = Instant::now();
        let is_double_click =
            self.last_titlebar_click
                .as_ref()
                .is_some_and(|(last_id, last_time, last_pos)| {
                    let d = position - *last_pos;
                    *last_id == id
                        && now.saturating_duration_since(*last_time) <= DOUBLE_CLICK_MAX
                        && d.x * d.x + d.y * d.y <= DOUBLE_CLICK_DISTANCE_SQ
                });

        self.last_titlebar_click = Some((id, now, position));

        if is_double_click {
            self.last_titlebar_click = None;
            self.pending_compositor_move = None;
            self.pending_xdg_move = None;
            self.input.pointer_left_down = false;
            self.suppress_next_left_release = true;
            self.toggle_maximize(id);
            return true;
        }

        false
    }

    fn try_begin_compositor_move(&mut self, id: WindowId) {
        if self.toplevel_pointer.is_some() {
            return;
        }
        let Some(w) = self.window(id) else {
            return;
        };
        if !w.mapped || w.maximized || w.fullscreen || w.minimized {
            return;
        }
        self.request_move(id);
    }

    fn try_begin_compositor_resize(&mut self, id: WindowId, edge: ResizeEdge) {
        if self.toplevel_pointer.is_some() {
            return;
        }
        let Some(w) = self.window(id) else {
            return;
        };
        if !w.mapped || w.maximized || w.fullscreen || w.minimized {
            return;
        }
        self.focus_window_id(id);
        self.clear_client_pointer_focus(self.pointer_pos);
        self.request_resize(id, edge);
    }

    /// Top-most mapped window resize edge at `position` (work area only).
    fn top_window_resize_edge_at(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(WindowId, ResizeEdgeMask)> {
        if !self.pointer_in_work_recess(position) {
            return None;
        }
        let px = position.x.round() as i32;
        let py = position.y.round() as i32;
        let ws = self.focused_workspace();
        self.space.elements().rev().find_map(|window| {
            let w = self
                .windows
                .iter()
                .find(|mw| mw.mapped && mw.workspace == ws && &mw.window == window)?;
            if !w.mapped || w.maximized || w.fullscreen || w.minimized {
                return None;
            }
            window.x11_surface()?;
            let bbox = self.global_window_bbox(window)?;
            let edges = resize_edges_at(bbox, px, py, RESIZE_BORDER_PX)?;
            Some((w.id, edges))
        })
    }

    fn update_pointer_cursor(&mut self, position: Point<f64, Logical>) {
        if self
            .dnd_cursor_phase
            .as_ref()
            .is_some_and(|phase| phase.load(Ordering::Relaxed) == DND_CURSOR_ENDED)
        {
            self.dnd_cursor_phase = None;
        }

        if let Some(icon) = self.dnd_cursor_icon(position) {
            self.set_flow_cursor_icon(icon);
            return;
        }

        if let Some(interaction) = &self.toplevel_pointer {
            let icon = match interaction {
                ToplevelPointerInteraction::Resize { edges, .. } => cursor_for_resize_edges(*edges),
                ToplevelPointerInteraction::Move { .. } => CursorIcon::Move,
            };
            self.cursor_manager.set_icon(icon);
            return;
        }

        if self.render.egui.has_open_panels() || self.active_dialog.is_some() {
            return;
        }

        if let Some((_, edges)) = self.top_window_resize_edge_at(position) {
            self.cursor_manager.set_icon(cursor_for_resize_edges(edges));
        } else if self.pending_compositor_move.is_some() {
            self.cursor_manager.set_icon(CursorIcon::Move);
        } else {
            self.cursor_manager.set_icon(CursorIcon::Default);
        }
    }

    pub(crate) fn begin_dnd_cursor(&mut self, phase: Arc<AtomicU8>) {
        phase.store(DND_CURSOR_FILE, Ordering::Relaxed);
        self.dnd_cursor_phase = Some(phase);
        self.set_flow_cursor_icon(FlowCursorIcon::FileDrag);
    }

    pub(crate) fn end_dnd_cursor(&mut self) {
        self.dnd_cursor_phase = None;
        self.set_flow_cursor_icon(FlowCursorIcon::Default);
    }

    fn dnd_cursor_icon(&self, position: Point<f64, Logical>) -> Option<FlowCursorIcon> {
        let phase_cell = self.dnd_cursor_phase.as_ref()?;
        if phase_cell.load(Ordering::Relaxed) == DND_CURSOR_FILE
            && self.pointer_surface_under(position).is_none()
        {
            phase_cell.store(DND_CURSOR_INVALID, Ordering::Relaxed);
        }

        let phase = phase_cell.load(Ordering::Relaxed);
        match phase {
            DND_CURSOR_VALID => Some(FlowCursorIcon::FileDragCopy),
            DND_CURSOR_INVALID => Some(FlowCursorIcon::NotAllowed),
            _ => Some(FlowCursorIcon::FileDrag),
        }
    }

    pub(crate) fn set_flow_cursor_icon(&mut self, icon: FlowCursorIcon) {
        if self.cursor_manager.current_flow_icon() == icon {
            return;
        }
        self.cursor_manager.set_flow_icon(icon);
        self.drm_submit_hw_cursor = true;
        self.mark_focused_output_full_damage(DamageSource::Cursor);
    }

    pub(crate) fn focus_window_id(&mut self, window_id: WindowId) {
        let Some(idx) = self.windows.iter().position(|w| w.id == window_id) else {
            return;
        };

        let old_focus_bbox = self
            .focused_window
            .and_then(|id| self.window(id))
            .and_then(|managed| self.global_window_bbox(&managed.window));

        self.focused_window = Some(window_id);
        let window = self.windows[idx].window.clone();
        self.space.raise_element(&window, true);
        let new_focus_bbox = self.global_window_bbox(&window);

        // `raise_element(..., true)` updates xdg `Activated` in pending state only; clients are not
        // notified until configure is sent. Without this, keyboard enter/leave can be ignored for
        // text input until something else triggers a configure (e.g. closing the other window).
        for managed in &self.windows {
            if let Some(tl) = managed.window.toplevel() {
                let _ = tl.send_pending_configure();
            }
        }

        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Some(KeyboardFocusTarget::Window(window)), serial);
        }

        if let Some(rect) = old_focus_bbox {
            self.mark_window_bbox_damage_source(rect, DamageSource::Unknown);
        }
        if let Some(rect) = new_focus_bbox {
            self.mark_window_bbox_damage_source(rect, DamageSource::Unknown);
        }
    }

    fn focus_window_at(&mut self, position: Point<f64, Logical>) {
        let px = position.x.round() as i32;
        let py = position.y.round() as i32;
        let ws = self.focused_workspace();
        let target_id = self.space.elements().rev().find_map(|window| {
            let managed = self
                .windows
                .iter()
                .find(|mw| mw.mapped && mw.workspace == ws && &mw.window == window)?;
            self.global_window_bbox(window)
                .is_some_and(|bbox| bbox.contains((px, py)))
                .then_some(managed.id)
        });

        if let Some(id) = target_id {
            self.focus_window_id(id);
        }
    }

    /// Cycle keyboard focus among mapped windows in compositor stacking order (bottom → top).
    fn cycle_focused_window(&mut self, delta: isize) {
        let ids: Vec<WindowId> = self
            .space
            .elements()
            .filter_map(|w| {
                self.windows
                    .iter()
                    .find(|mw| mw.mapped && &mw.window == w)
                    .map(|mw| mw.id)
            })
            .collect();

        if ids.len() < 2 {
            return;
        }

        let len = ids.len() as isize;
        let idx = self
            .focused_window
            .and_then(|id| ids.iter().position(|&x| x == id))
            .map(|i| i as isize)
            .unwrap_or(len - 1);

        let next = (idx + delta).rem_euclid(len) as usize;
        self.focus_window_id(ids[next]);
    }

    pub fn new(init: DesktopInit) -> Self {
        let debug = init.debug.clone();
        apply_debug_log_level(debug.log_level);

        Self {
            fonts: FontSystem::new(BuiltInThemeId::Classic).expect("REASON"),
            dialogs: Vec::new(),
            active_dialog: None,
            display_handle: init.display_handle,
            xdg_activation_state: init.xdg_activation_state,
            #[cfg(feature = "xwayland")]
            xwayland_shell_state: init.xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xwayland_client: None,
            #[cfg(feature = "xwayland")]
            xwayland_display: None,
            #[cfg(feature = "xwayland")]
            xwayland_loop_handle: None,
            winit_scale_factor: 1.0,
            ui: UiTree::default(),
            active_workspace: WorkspaceId(1),
            workspace_names: vec!["Workspace 1".to_string()],
            next_window_id: WindowId(1),
            primary_output: init.primary_output,
            focused_output: init.primary_output,
            focus_changed_at: Instant::now(),
            input: InputState::default(),
            compositor_state: init.compositor_state,
            render: init.render,
            xdg_shell_state: init.xdg_shell_state,
            dmabuf_state: init.dmabuf_state,
            dmabuf_global: None,
            dmabuf_node: None,
            portal_dmabuf_formats: Vec::new(),
            shm_state: init.shm_state,
            seat_state: init.seat_state,
            output_manager_state: init.output_manager_state,
            data_device_state: init.data_device_state,
            primary_selection_state: init.primary_selection_state,
            layer_shell_state: init.layer_shell_state,
            image_capture_source_state: init.image_capture_source_state,
            output_capture_source_state: init.output_capture_source_state,
            image_copy_capture_state: init.image_copy_capture_state,
            image_copy_capture_sessions: Vec::new(),
            color_tag_state: init.color_tag_state,
            color_management_state: init.color_management_state,
            portal_dispatch_ctx: None,
            pending_portal_captures: Vec::new(),
            pending_ai_permission_responses: HashMap::new(),
            portal_frame_cache: HashMap::new(),
            portal_capture_source: HashMap::new(),
            portal_offscreen_targets: HashMap::new(),
            compositor_ready: false,
            backend_kind: init.backend_kind,
            cursor_manager: init.cursor_manager,
            seat: init.seat,
            chrome: init.chrome,
            space: Space::default(),
            popups: PopupManager::default(),
            windows: Vec::new(),
            outputs: IndexMap::<OutputId, OutputState>::new(),
            current_workspace: 0,

            seat_name: "seat-0".to_string(),
            focused_window: None,
            pointer_pos: (0.0, 0.0).into(),
            toplevel_pointer: None,
            dnd_cursor_phase: None,

            notifications: init.notifications,
            lock_screen: LockScreenState::new(),
            unmapped_windows: Vec::new(),
            keybinds: init.keybinds,
            client_wayland_display: init.client_wayland_display,
            apps: init.apps,
            privacy: init.privacy,
            debug: debug.clone(),
            settings_ipc_rx: start_desktop_settings_ipc(),
            settings_ipc_watchers: Vec::new(),
            settings_ipc_config: load_config(),
            host_window_drag_requested: false,
            pending_compositor_move: None,
            pending_xdg_move: None,
            last_titlebar_click: None,
            suppress_next_left_release: false,
            running: init.running,
            drm_cursor_render_id: Id::new(),
            drm_submit_hw_cursor: false,
            drm_try_pass_cursor_this_frame: false,
            cursor_owner_output: None,
            screenshot_requested: None,
            screenshot_all_requested: false,
            screenshot_seq: 0,
            theme: init.theme_manager,
            surface_colors: HashMap::new(),
            damage_debug_enabled: debug_damage_enabled(&debug),
            damage_source_counts: DamageSourceCounts::default(),
            sidebar_pulse: None,
            topbar_pulse: None,
            flow_field_pulse: None,
            clock_pulse: None,
            ai_flow_mode_cache: AiFlowMode::Idle,
            ai_flow_mode_last_poll: Instant::now(),
            ui_sound_player: UiSoundPlayer::new(),
            last_sidebar_hover_sound_target: None,
            last_clock_text: String::new(),
            next_dialog_id: 1,
        }
    }

    pub fn alloc_window_id(&mut self) -> WindowId {
        let id = self.next_window_id.0;
        self.next_window_id = WindowId(id.checked_add(1).expect("window id counter overflowed"));
        WindowId(id)
    }

    pub fn add_xdg_toplevel(&mut self, surface: ToplevelSurface) {
        let id = self.alloc_window_id();
        let _span = info_span!(
            "add_xdg_toplevel",
            session_id = session_id(),
            window_id = ?id,
            surface = ?surface.wl_surface().id(),
            windows = self.windows.len(),
            space = self.space.elements().count()
        )
        .entered();

        let window = Window::new_wayland_window(surface.clone());
        let workspace = self.focused_workspace();
        let meta = WaylandWindowMeta::new(None, None);

        //self.space.map_element(window.clone(), (100, 100), false);
        //dbg_flush(&format!("after map space={}", self.space.elements().count()));

        let managed = ManagedWindow::new_wayland(id, window.clone(), meta, workspace);
        self.windows.push(managed);
        trace!(
            target: "focaldesk",
            window_id = ?id,
            windows = self.windows.len(),
            "wayland toplevel added"
        );

        self.mark_focused_output_full_damage(DamageSource::Unknown);
    }

    #[cfg(feature = "xwayland")]
    pub fn add_xwayland_window(
        &mut self,
        surface: smithay::xwayland::X11Surface,
        override_redirect: bool,
    ) -> WindowId {
        let id = self.alloc_window_id();
        let _span = info_span!(
            "add_xwayland_window",
            session_id = session_id(),
            window_id = ?id,
            title = ?surface.title(),
            class = ?surface.class(),
            override_redirect
        )
        .entered();
        let window = Window::new_x11_window(surface.clone());
        let workspace = self.focused_workspace();
        let meta = XwaylandWindowMeta::from_surface(&surface)
            .with_override_redirect(override_redirect)
            .with_role(XwaylandSurfaceRole::from_surface(&surface));
        let mut managed = ManagedWindow::new_xwayland(id, window, meta.clone(), workspace);
        managed.floating = meta.should_float();
        managed.mapped = false;
        self.windows.push(managed);
        trace!(
            target: "focaldesk",
            window_id = ?id,
            windows = self.windows.len(),
            "xwayland window added"
        );
        id
    }

    #[cfg(feature = "xwayland")]
    pub fn window_id_for_x11_surface(
        &self,
        surface: &smithay::xwayland::X11Surface,
    ) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|managed| {
                managed
                    .window
                    .x11_surface()
                    .map(|x11| x11 == surface)
                    .unwrap_or(false)
            })
            .map(|managed| managed.id)
    }

    #[cfg(feature = "xwayland")]
    pub fn sync_xwayland_window_meta(&mut self, surface: &smithay::xwayland::X11Surface) {
        let Some(id) = self.window_id_for_x11_surface(surface) else {
            return;
        };
        let Some(window) = self.window_mut(id) else {
            return;
        };
        if let crate::core::shell::managed_window::ManagedWindowKind::Xwayland(meta) =
            &mut window.kind
        {
            *meta = XwaylandWindowMeta::from_surface(surface)
                .with_override_redirect(surface.is_override_redirect())
                .with_role(XwaylandSurfaceRole::from_surface(surface));
            window.floating = meta.should_float();
        }
    }

    #[cfg(feature = "xwayland")]
    pub fn map_xwayland_window(&mut self, surface: smithay::xwayland::X11Surface) {
        let id = self.window_id_for_x11_surface(&surface).unwrap_or_else(|| {
            self.add_xwayland_window(surface.clone(), surface.is_override_redirect())
        });
        let _span = info_span!(
            "map_xwayland_window",
            session_id = session_id(),
            window_id = ?id,
            title = ?surface.title(),
            class = ?surface.class(),
            override_redirect = surface.is_override_redirect()
        )
        .entered();

        self.sync_xwayland_window_meta(&surface);

        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(true);
        }

        let Some(idx) = self.windows.iter().position(|window| window.id == id) else {
            return;
        };

        if surface.wl_surface().is_none() {
            self.mark_window_id_damage(id, DamageSource::CommitBbox);
            let window = self.windows[idx].window.clone();
            self.space.unmap_elem(&window);
            self.windows[idx].mapped = false;
            debug!(
                target: "focaldesk",
                window_id = ?id,
                "xwayland map deferred: no associated wl_surface yet"
            );
            return;
        }

        let window = self.windows[idx].window.clone();
        let requested_geometry = surface.geometry();
        let output_id = self
            .window_id_for_x11_surface(&surface)
            .map(|window_id| self.xwayland_output_id_for_window(window_id))
            .unwrap_or_else(|| {
                self.output_under_pointer(self.input.pointer_pos)
                    .unwrap_or(self.primary_output)
            });

        let should_float = self.windows[idx].floating;
        let (bbox_location, configure_size, maximize_on_map) = if surface.is_override_redirect() {
            let geometry = Rectangle::from_loc_and_size(
                self.xwayland_or_compositor_loc(&surface, requested_geometry.loc),
                requested_geometry.size,
            );
            let geometry = self.xwayland_clamp_override_redirect_geometry(output_id, geometry);
            (geometry.loc, geometry.size, false)
        } else if should_float {
            (
                self.default_toplevel_map_location(output_id),
                requested_geometry.size,
                false,
            )
        } else {
            let work = self
                .work_recess_for_output(output_id)
                .unwrap_or(requested_geometry);
            (work.loc, work.size, true)
        };

        if maximize_on_map {
            self.windows[idx].set_maximized(true);
        }

        self.windows[idx].float_rect =
            Some(Rectangle::from_loc_and_size(bbox_location, configure_size));

        window.on_commit();
        self.map_window_bbox_location(window.clone(), bbox_location, true);
        self.windows[idx].mapped = true;
        trace!(
            target: "focaldesk",
            window_id = ?id,
            ?bbox_location,
            "xwayland window mapped"
        );

        if !surface.is_override_redirect() {
            let configure_rect = Rectangle::from_loc_and_size(bbox_location, configure_size);
            debug!(
                target: "focaldesk",
                window_id = ?id,
                requested_geometry = ?requested_geometry,
                configure_rect = ?configure_rect,
                "xwayland configure"
            );
            let _ = surface.configure(Some(configure_rect));
            if maximize_on_map {
                let _ = surface.set_maximized(true);
            }
            self.focus_window_id(id);
        }

        self.space.refresh();
        self.mark_window_id_damage(id, DamageSource::CommitBbox);
    }

    pub fn open_dialog(&mut self, dialog: Dialog) {
        self.active_dialog = Some(dialog.id);
        self.dialogs.push(dialog);

        tracing::info!(
            target: "focaldesk",
            session_id = session_id(),
            dialogs = self.dialogs.len(),
            active_dialog = ?self.active_dialog,
            "dialog opened"
        );

        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    fn alloc_dialog_id(&mut self) -> DialogId {
        let id = self.next_dialog_id;
        self.next_dialog_id = self
            .next_dialog_id
            .checked_add(1)
            .expect("dialog id counter overflowed");
        id
    }

    fn open_ai_permission_dialog(
        &mut self,
        request_id: u64,
        title: String,
        message: String,
        allow_persistent: bool,
        response: mpsc::Sender<IpcResponse>,
    ) -> DialogId {
        let dialog_id = self.alloc_dialog_id();
        let owner_output = self.focused_output;
        let output = self
            .outputs
            .get(&owner_output)
            .expect("AI permission dialog requires at least one output");
        let screen = Rectangle::from_loc_and_size((0, 0), output.logical_size);
        let helper = AiPermissionDialog::new(request_id, title, message, allow_persistent);
        let dialog = helper.to_dialog(dialog_id, owner_output, screen);

        self.pending_ai_permission_responses
            .insert(dialog_id, (request_id, response));
        self.open_dialog(dialog);
        dialog_id
    }

    pub fn close_dialog(&mut self, id: DialogId) {
        self.dialogs.retain(|d| d.id != id);

        if self.active_dialog == Some(id) {
            self.active_dialog = None;
        }

        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    pub fn handle_dialog_action(&mut self, id: DialogId, action: DialogAction) {
        let maybe_ai_response =
            self.pending_ai_permission_responses
                .remove(&id)
                .map(|(request_id, response)| {
                    let prompt = AiPermissionDialog::response_for_action(action);
                    let _ = response.send(IpcResponse::AiPermissionDecision {
                        request_id,
                        allow: matches!(
                            prompt.decision,
                            focaldesk_permissions::PermissionDecision::Allow
                        ),
                        persistent: matches!(
                            prompt.scope,
                            focaldesk_permissions::PermissionScope::Persistent
                        ),
                    });
                });

        match action {
            DialogAction::Confirm => {
                tracing::info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    dialog_id = ?id,
                    action = "confirm",
                    "dialog action"
                );
            }

            DialogAction::Cancel => {
                tracing::info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    dialog_id = ?id,
                    action = "cancel",
                    "dialog action"
                );
            }

            DialogAction::Custom(v) => {
                tracing::info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    dialog_id = ?id,
                    action = "custom",
                    value = %v,
                    "dialog action"
                );
            }
        }

        self.close_dialog(id);
        let _ = maybe_ai_response;
    }

    /// Import the committed surface tree into the active renderer (during Wayland dispatch).
    pub fn early_import_surface(&mut self, surface: &WlSurface) {
        let Some(ctx) = self.portal_dispatch_ctx.as_mut() else {
            return;
        };
        // SAFETY: only called synchronously from `dispatch_clients` while ctx is set.
        let renderer = unsafe { &mut *ctx.renderer.as_ptr() };
        if let Err(err) = import_surface_tree(renderer, surface) {
            tracing::error!(
                target: "focaldesk",
                session_id = session_id(),
                error = ?err,
                "early surface import failed"
            );
        }
    }

    pub fn handle_commit(&mut self, surface: &WlSurface) {
        tracing::trace!(
            target: "focaldesk",
            session_id = session_id(),
            surface = ?surface.id(),
            "handle_commit"
        );

        self.refresh_surface_color(surface);
        self.popups.commit(surface);

        let mut to_map: Option<usize> = None;
        let mut committed_window: Option<Window> = None;
        let mut commit_damage_queued = false;

        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        if let Some(window) = self.window_for_wl_surface(&root) {
            committed_window = Some(window.clone());
            window.on_commit();

            if window.x11_surface().is_some() && !is_sync_subsurface(surface) && &root == surface {
                let old_bbox = self.global_window_bbox(&window);
                let buffer_offset = with_states(surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take()
                });
                if let Some(buffer_offset) = buffer_offset {
                    let maximized = self
                        .windows
                        .iter()
                        .find(|managed| managed.window == window)
                        .is_some_and(|managed| managed.maximized);
                    if !maximized {
                        if let Some(current_loc) = self.space.element_location(&window) {
                            tracing::info!(
                                target: "focaldesk",
                                session_id = session_id(),
                                window_id = ?self.window_id_for_wl_surface(&root),
                                buffer_offset = ?buffer_offset,
                                current_loc = ?current_loc,
                                "xwayland buffer delta"
                            );
                            self.map_window_bbox_location(
                                window.clone(),
                                current_loc - window.geometry().loc + buffer_offset,
                                false,
                            );
                            if let Some(old_bbox) = old_bbox {
                                self.mark_window_bbox_damage_source(
                                    old_bbox,
                                    DamageSource::CommitBbox,
                                );
                            }
                            if let Some(new_bbox) = self.global_window_bbox(&window) {
                                self.mark_window_bbox_damage_source(
                                    new_bbox,
                                    DamageSource::CommitBbox,
                                );
                            }
                            commit_damage_queued = true;
                        }
                    }
                }
            }

            if let Some(idx) = self
                .windows
                .iter()
                .position(|managed| managed.window == window)
            {
                tracing::trace!(
                    target: "focaldesk",
                    session_id = session_id(),
                    window_id = ?self.windows[idx].id,
                    idx,
                    in_space = self.space.elements().any(|e| e == &window),
                    mapped = self.windows[idx].mapped,
                    "commit matched managed window"
                );
                let in_space = self.space.elements().any(|e| e == &window);
                if in_space && !self.windows[idx].mapped && !self.windows[idx].minimized {
                    self.windows[idx].mapped = true;
                    let window_id = self.windows[idx].id;
                    self.focus_window_id(window_id);
                    tracing::trace!(
                        target: "focaldesk",
                        session_id = session_id(),
                        window_id = ?window_id,
                        "existing space window marked mapped"
                    );
                } else if window.x11_surface().is_some() {
                    if !in_space {
                        to_map = Some(idx);
                    }
                } else if !in_space {
                    to_map = Some(idx);
                }
            }
        } else {
            for (idx, managed) in self.windows.iter().enumerate() {
                let mut belongs = false;
                managed.window.with_surfaces(|s, _| {
                    if s == surface {
                        belongs = true;
                    }
                });

                if belongs {
                    committed_window = Some(managed.window.clone());
                    managed.window.on_commit();
                    tracing::trace!(
                        target: "focaldesk",
                        session_id = session_id(),
                        idx,
                        "commit matched surface"
                    );
                    if !self.space.elements().any(|e| e == &managed.window) {
                        to_map = Some(idx);
                    }
                    break;
                }
            }
        }

        let mut mapped_window = false;
        if let Some(idx) = to_map {
            let window = self.windows[idx].window.clone();

            let output_id = self
                .output_under_pointer(self.input.pointer_pos)
                .unwrap_or(self.primary_output);

            let map_loc = self.windows[idx]
                .float_rect
                .map(|rect| rect.loc)
                .unwrap_or_else(|| self.default_toplevel_map_location(output_id));

            self.map_window_bbox_location(window, map_loc, false);
            self.windows[idx].mapped = true;
            let window_id = self.windows[idx].id;
            self.focus_window_id(window_id);
            tracing::trace!(
                target: "focaldesk",
                session_id = session_id(),
                window_id = ?window_id,
                space_count = self.space.elements().count(),
                "window mapped from commit"
            );
            mapped_window = true;
        }

        let resize_damage = handle_resize_surface_commit(&mut self.space, surface);
        if let Some((old_bbox, new_bbox)) = resize_damage {
            self.mark_window_bbox_damage_source(old_bbox, DamageSource::WindowResize);
            self.mark_window_bbox_damage_source(new_bbox, DamageSource::WindowResize);
        }

        self.ensure_popup_initial_configure(surface);

        if mapped_window {
            if let Some(window) = committed_window.as_ref() {
                if let Some(bbox) = self.global_window_bbox(window) {
                    self.mark_window_bbox_damage_source(bbox, DamageSource::CommitBbox);
                    commit_damage_queued = true;
                }
            }

            if !commit_damage_queued {
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }
        } else if resize_damage.is_none() {
            if !commit_damage_queued {
                if let Some(window) = committed_window.as_ref() {
                    if let Some(bbox) = self.global_window_bbox(window) {
                        self.mark_window_bbox_damage_source(bbox, DamageSource::CommitBbox);
                        commit_damage_queued = true;
                    }
                }
            }

            if !commit_damage_queued {
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }
        }
    }

    pub(crate) fn window_for_wl_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(surface))
            .cloned()
    }

    /// Clamp pending popup geometry to the union of outputs that contain the parent window.
    pub(crate) fn unconstrain_popup(&self, popup: &PopupSurface) {
        let popup_kind = PopupKind::from(popup.clone());
        let popup_toplevel_coords = get_popup_toplevel_coords(&popup_kind);

        // Nested popups already have a client-chosen direction relative to their parent popup.
        // Let the client keep that placement unless it explicitly repositions again.
        if popup_toplevel_coords != (0, 0).into() {
            return;
        }

        let Ok(root) = find_popup_root_surface(&popup_kind) else {
            return;
        };
        let Some(window) = self.window_for_wl_surface(&root) else {
            return;
        };

        let outputs_for_window = self.space.outputs_for_element(&window);
        if outputs_for_window.is_empty() {
            return;
        }

        let window_geo = self.space.element_geometry(&window).unwrap();

        let mut target: Option<Rectangle<i32, Logical>> = None;
        for output in &outputs_for_window {
            let Some(output_id) = self.output_id_for_space_output(output) else {
                continue;
            };
            let Some(work) = self.work_recess_for_output(output_id) else {
                continue;
            };
            let parent_relative =
                Rectangle::from_loc_and_size(work.loc - window_geo.loc, work.size);
            target = Some(match target {
                None => parent_relative,
                Some(existing) => existing.merge(parent_relative),
            });
        }

        let target = target.unwrap_or_else(|| {
            let mut outputs_geo = self.space.output_geometry(&outputs_for_window[0]).unwrap();
            for output in outputs_for_window.iter().skip(1) {
                outputs_geo = outputs_geo.merge(self.space.output_geometry(output).unwrap());
            }
            let mut fallback = outputs_geo;
            fallback.loc -= popup_toplevel_coords;
            fallback.loc -= window_geo.loc;
            fallback
        });
        if target.size.w <= 0 || target.size.h <= 0 {
            return;
        }

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    fn ensure_popup_initial_configure(&mut self, surface: &WlSurface) {
        let Some(popup) = self.popups.find_popup(surface) else {
            return;
        };
        let PopupKind::Xdg(popup) = popup else {
            return;
        };
        if !popup.is_initial_configure_sent() {
            let _ = popup.send_configure();
        }
    }

    pub fn handle_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::QuitCompositor => {
                tracing::info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    action = "quit_compositor",
                    "quit compositor"
                );
                self.running = false;
            }

            KeyAction::CloseFocused => {
                self.close_focused();
            }

            KeyAction::FocusNext => {
                self.cycle_focused_window(1);
            }

            KeyAction::FocusPrev => {
                self.cycle_focused_window(-1);
            }

            KeyAction::LaunchTerminal => {
                self.launch_app(self.apps.terminal.clone());
            }

            KeyAction::LockScreen => {
                self.lock_session();
            }

            KeyAction::ToggleLauncher => {
                self.render
                    .egui
                    .open_panel(PanelKind::AppLauncher, self.focused_output);
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }

            KeyAction::ActivateSlot(n) => {
                flog_warn!("Activate slot {} (not implemented yet)", n);
            }

            KeyAction::AssignSlot(n) => {
                flog_warn!("Assign slot {} (not implemented yet)", n);
            }

            KeyAction::OverflowView => {
                flog_warn!("Overflow view (not implemented yet)");
            }

            KeyAction::TakeScreenshot => {
                self.request_screenshot();
                tracing::debug!(
                    target: "focaldesk",
                    session_id = session_id(),
                    action = "take_screenshot",
                    "screenshot action fired"
                );
            }

            KeyAction::TakeScreenshotAll => {
                self.request_screenshot_all();
                tracing::debug!(
                    target: "focaldesk",
                    session_id = session_id(),
                    action = "take_screenshot_all",
                    "screenshot-all action fired"
                );
            }

            KeyAction::LaunchBrowser => {
                todo!();
            }

            KeyAction::LaunchFiles => {
                self.launch_app(focaldesk_files_command());
            }
        }
    }

    pub fn close_focused(&mut self) {
        let Some(focused_id) = self.focused_window else {
            return;
        };

        let Some(window) = self
            .window(focused_id)
            .map(|managed| managed.window.clone())
        else {
            self.focused_window = None;
            return;
        };

        if let Some(managed) = self.window(focused_id) {
            managed.request_close();
        }
        if let Some(bbox) = self.global_window_bbox(&window) {
            self.mark_window_bbox_damage_source(bbox, DamageSource::Unknown);
        }
    }

    pub fn activate_slot(&mut self, slot: usize) {
        flog_warn!("Activate slot {} (not implemented yet)", slot);
    }

    pub fn assign_slot(&mut self, slot: usize) {
        flog_warn!("Assign slot {} (not implemented yet)", slot);
    }

    fn update_focus(&mut self) {}

    pub fn launch_terminal(&self) {
        self.launch_app(self.apps.terminal.clone());
    }

    /// True when it is safe to run [`wayland_server::Display::dispatch_clients`].
    /// While the XWayland Wayland client is connected but the X11 WM is not attached yet,
    /// dispatching would panic in smithay's XWayland shell commit hook.
    #[cfg(feature = "xwayland")]
    pub fn wayland_clients_may_dispatch(&self) -> bool {
        self.xwayland_client.is_none() || self.xwm.is_some()
    }

    #[cfg(not(feature = "xwayland"))]
    pub fn wayland_clients_may_dispatch(&self) -> bool {
        true
    }

    /// Tear down a failed or exited XWayland instance so normal Wayland clients can run.
    #[cfg(feature = "xwayland")]
    pub fn disable_xwayland(&mut self) {
        use smithay::reexports::wayland_server::backend::DisconnectReason;

        if let Some(client) = self.xwayland_client.take() {
            let _ = self
                .display_handle
                .backend_handle()
                .kill_client(client.id(), DisconnectReason::ConnectionClosed);
        }
        self.xwm = None;
        self.xwayland_display = None;
        flog("XWayland disabled");
    }

    pub fn launch_app(&self, app: String) {
        let app_name = app.clone();
        let chrome_like = is_chrome_like(&app_name);
        let browser_like = is_browser_like(&app_name);
        let cursor_like = is_cursor_like(&app_name);
        #[cfg(feature = "xwayland")]
        let xwayland_display = self.xwayland_display.as_deref();

        #[cfg(not(feature = "xwayland"))]
        let xwayland_display: Option<&str> = None;

        let launch_span = tracing::info_span!(
            "launch_app",
            session_id = session_id(),
            app = %app_name,
            chrome_like,
            browser_like,
            cursor_like,
            backend = ?self.backend_kind,
            wayland_display = %self.client_wayland_display,
            xwayland_display = ?xwayland_display
        );
        let _enter = launch_span.enter();
        chrome_launch_note(format!(
            "launch_app app={app_name} chrome_like={chrome_like} browser_like={browser_like} pid={} client_wayland_display={} xwayland_display={:?}",
            id(),
            self.client_wayland_display,
            xwayland_display
        ));

        let display_env = xwayland_display.map(str::to_string);
        let launch_candidates = if chrome_like {
            chrome_exec_fallbacks(&app_name)
        } else {
            vec![app_name.clone()]
        };
        chrome_launch_note(format!(
            "launch candidates for {app_name}: {:?}",
            launch_candidates
        ));

        let mut last_error = None;
        for candidate in launch_candidates {
            chrome_launch_note(format!("trying candidate={candidate}"));
            let mut command = Command::new(&candidate);

            let browser_backend = self.apps.browser_launch_backend;
            let prefer_x11 = match browser_backend {
                BrowserLaunchBackend::Auto => false,
                BrowserLaunchBackend::Wayland => false,
                BrowserLaunchBackend::Xwayland => true,
            };
            if chrome_like || browser_like {
                if prefer_x11 && xwayland_display.is_some() {
                    command.env_remove("WAYLAND_DISPLAY");
                    if let Some(display) = xwayland_display {
                        command.env("DISPLAY", display);
                    }
                } else {
                    command.env("WAYLAND_DISPLAY", &self.client_wayland_display);
                    command.env_remove("DISPLAY");
                }
                chrome_launch_note(format!(
                    "browser backend policy for {app_name}: {:?} prefer_x11={prefer_x11}",
                    browser_backend
                ));
            } else {
                command.env("WAYLAND_DISPLAY", &self.client_wayland_display);
                command.env_remove("DISPLAY");
                if let Some(display) = xwayland_display {
                    command.env("DISPLAY", display);
                }
            }
            if chrome_like || cursor_like {
                configure_chrome_command(&mut command, prefer_x11);
                if let Some(file) = open_session_log_file() {
                    if let Ok(stderr_file) = file.try_clone() {
                        command.stdout(Stdio::from(file));
                        command.stderr(Stdio::from(stderr_file));
                    }
                }
            }
            if is_obs_like(&candidate) {
                configure_obs_recording_dir();
            }

            match command.spawn() {
                Ok(mut child) => {
                    let child_pid = child.id();
                    chrome_launch_note(format!(
                        "spawned candidate={candidate} pid={child_pid} wayland_display={} display={display_env:?}",
                        self.client_wayland_display,
                    ));
                    let trace_candidate = candidate.clone();
                    thread::spawn(move || {
                        match child.wait() {
                            Ok(status) => chrome_launch_note(format!(
                                "child exited candidate={trace_candidate} pid={child_pid} status={status}"
                            )),
                            Err(err) => chrome_launch_note(format!(
                                "child wait failed candidate={trace_candidate} pid={child_pid} err={err}"
                            )),
                        }
                    });
                    if xwayland_display.is_none() && !(chrome_like || cursor_like) {
                        tracing::warn!(
                            target: "focaldesk",
                            session_id = session_id(),
                            candidate = %candidate,
                            "launched without DISPLAY; X11 apps need XWayland"
                        );
                    }
                    return;
                }
                Err(err) => {
                    chrome_launch_note(format!("spawn failed candidate={candidate} err={err}"));
                    last_error = Some((candidate, err));
                }
            }
        }

        if let Some((candidate, err)) = last_error {
            chrome_launch_note(format!(
                "all launch candidates failed last_candidate={candidate} err={err}"
            ));
        }
    }

    pub fn handle_key_event(&mut self, keycode: u32, state: FlowKeyState) {
        use smithay::backend::input::KeyState as SmithayKeyState;
        use smithay::input::keyboard::ModifiersState;

        let smithay_state = match state {
            FlowKeyState::Pressed => SmithayKeyState::Pressed,
            FlowKeyState::Released => SmithayKeyState::Released,
        };

        let serial = SERIAL_COUNTER.next_serial();
        let time = 0;

        let Some(keyboard) = self.seat.get_keyboard() else {
            flog_info!("no keyboard on seat");
            return;
        };

        let keybinds = self.keybinds.clone();
        let mut resolved_action = None;

        keyboard.input(
            self,
            keycode.into(),
            smithay_state,
            serial,
            time,
            |ds, mods: &ModifiersState, handle| {
                let sym = handle.modified_sym().raw();

                let mut mask = ModMask::empty();
                if mods.shift {
                    mask |= ModMask::SHIFT;
                }
                if mods.ctrl {
                    mask |= ModMask::CTRL;
                }
                if mods.alt {
                    mask |= ModMask::ALT;
                }
                if mods.logo {
                    mask |= ModMask::SUPER;
                }

                flog(format!(
                    "KEY DEBUG: keycode={} sym={} state={:?} mods={:?}",
                    keycode, sym, state, mods
                ));

                // Modal dialogs: keyboard still updates XKB via `keyboard.input`, but compositor
                // shortcuts stay disabled and Wayland clients do not receive these events.
                if let Some(did) = ds.active_dialog {
                    if let Some(dialog) = ds.dialogs.iter().find(|d| d.id == did) {
                        match smithay_state {
                            SmithayKeyState::Pressed => {
                                if sym == keysyms::KEY_Escape && dialog.dismissible {
                                    ds.close_dialog(did);
                                    return FilterResult::<()>::Intercept(());
                                }
                                if sym == keysyms::KEY_Return || sym == keysyms::KEY_KP_Enter {
                                    let choice = dialog
                                        .buttons
                                        .iter()
                                        .find(|b| matches!(b.action, DialogAction::Confirm))
                                        .map(|b| b.action)
                                        .or_else(|| dialog.buttons.first().map(|b| b.action))
                                        .unwrap_or(DialogAction::Cancel);
                                    ds.handle_dialog_action(did, choice);
                                    return FilterResult::<()>::Intercept(());
                                }
                                // Other keys don't trigger compositor actions while open.
                                return FilterResult::<()>::Intercept(());
                            }
                            SmithayKeyState::Released => {
                                return FilterResult::<()>::Intercept(());
                            }
                        }
                    }
                }

                if sym == keysyms::KEY_Print && matches!(state, FlowKeyState::Released) {
                    if mask.contains(ModMask::SHIFT) {
                        resolved_action = Some(KeyAction::TakeScreenshotAll);
                    } else {
                        resolved_action = Some(KeyAction::TakeScreenshot);
                    }
                    return FilterResult::<()>::Intercept(());
                }

                if matches!(state, FlowKeyState::Pressed) {
                    resolved_action = keybinds.resolve(sym, mask);
                    if resolved_action.is_some() {
                        return FilterResult::<()>::Intercept(());
                    }
                }

                FilterResult::<()>::Forward
            },
        );

        if let Some(action) = resolved_action {
            flog(format!("ACTION={:?}", action,));

            self.handle_action(action);
        }
    }

    fn handle_lock_key_event(&mut self, keycode: u32, state: FlowKeyState) {
        use smithay::backend::input::KeyState as SmithayKeyState;
        use smithay::input::keyboard::ModifiersState;

        let smithay_state = match state {
            FlowKeyState::Pressed => SmithayKeyState::Pressed,
            FlowKeyState::Released => SmithayKeyState::Released,
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = 0;

        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };

        keyboard.input(
            self,
            keycode.into(),
            smithay_state,
            serial,
            time,
            |ds, mods: &ModifiersState, handle| {
                ds.input.modifiers = FlowModifiers {
                    shift: mods.shift,
                    ctrl: mods.ctrl,
                    alt: mods.alt,
                    super_key: mods.logo,
                };

                if !matches!(smithay_state, SmithayKeyState::Pressed) {
                    return FilterResult::<()>::Intercept(());
                }

                if ds.lock_screen.authenticating {
                    return FilterResult::<()>::Intercept(());
                }

                let sym = handle.modified_sym().raw();
                match sym {
                    keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                        ds.submit_lock_password();
                    }
                    keysyms::KEY_BackSpace => {
                        ds.lock_screen.backspace();
                        ds.mark_all_outputs_full_damage(DamageSource::Unknown);
                    }
                    keysyms::KEY_Escape => {
                        ds.lock_screen.clear_password();
                        ds.lock_screen.message = "Enter password".to_string();
                        ds.mark_all_outputs_full_damage(DamageSource::Unknown);
                    }
                    _ => {
                        let text = if ds.input.modifiers.ctrl
                            || ds.input.modifiers.alt
                            || ds.input.modifiers.super_key
                        {
                            String::new()
                        } else {
                            xkb::keysym_to_utf8(handle.modified_sym())
                        };

                        if !text.is_empty() {
                            for ch in text.chars() {
                                if !ch.is_control() {
                                    ds.lock_screen.push_char(ch);
                                }
                            }
                            ds.mark_all_outputs_full_damage(DamageSource::Unknown);
                        }
                    }
                }

                FilterResult::<()>::Intercept(())
            },
        );
    }

    fn process_toplevel_pointer_motion(&mut self, pos: Point<f64, Logical>) -> bool {
        match self.toplevel_pointer {
            Some(ToplevelPointerInteraction::Move {
                window_id,
                pointer_start,
                initial_location,
            }) => {
                let delta = pos - pointer_start;
                let new_loc = (initial_location.to_f64() + delta).to_i32_round();
                if let Some(w) = self.window(window_id) {
                    let window = w.window.clone();
                    let old_bbox = self.global_window_bbox(&window);
                    let new_loc =
                        self.clamp_window_location_to_work_recess(&w.window, new_loc, pos);
                    self.map_window_bbox_location(window, new_loc, false);
                    self.space.refresh();
                    let new_bbox = self
                        .window(window_id)
                        .and_then(|w| self.global_window_bbox(&w.window));
                    if let Some(old_bbox) = old_bbox {
                        self.mark_window_bbox_damage_source(old_bbox, DamageSource::WindowMove);
                    }
                    if let Some(new_bbox) = new_bbox {
                        self.mark_window_bbox_damage_source(new_bbox, DamageSource::WindowMove);
                    }
                    return old_bbox.is_some() || new_bbox.is_some();
                }
            }
            Some(ToplevelPointerInteraction::Resize {
                window_id,
                edges,
                pointer_start,
                initial_rect,
                ..
            }) => {
                let mut delta = pos - pointer_start;

                let mut new_window_width = initial_rect.size.w;
                let mut new_window_height = initial_rect.size.h;

                let e = edges;
                if e.intersects(ResizeEdgeMask::LEFT | ResizeEdgeMask::RIGHT) {
                    if e.intersects(ResizeEdgeMask::LEFT) {
                        delta.x = -delta.x;
                    }
                    new_window_width = (initial_rect.size.w as f64 + delta.x) as i32;
                }

                if e.intersects(ResizeEdgeMask::TOP | ResizeEdgeMask::BOTTOM) {
                    if e.intersects(ResizeEdgeMask::TOP) {
                        delta.y = -delta.y;
                    }
                    new_window_height = (initial_rect.size.h as f64 + delta.y) as i32;
                }

                let Some(w) = self.window(window_id) else {
                    return false;
                };
                let Some(tl) = w.window.toplevel() else {
                    return false;
                };

                let (min_size, max_size) = compositor::with_states(tl.wl_surface(), |states| {
                    let mut guard = states.cached_state.get::<SurfaceCachedState>();
                    let data = guard.current();
                    (data.min_size, data.max_size)
                });

                let min_width = min_size.w.max(1);
                let min_height = min_size.h.max(1);
                let max_width = if max_size.w == 0 {
                    i32::MAX
                } else {
                    max_size.w
                };
                let max_height = if max_size.h == 0 {
                    i32::MAX
                } else {
                    max_size.h
                };

                let last_window_size = Size::from((
                    new_window_width.max(min_width).min(max_width),
                    new_window_height.max(min_height).min(max_height),
                ));

                tl.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Resizing);
                    state.size = Some(last_window_size);
                });
                tl.send_pending_configure();

                if let Some(slot) = self.toplevel_pointer.as_mut() {
                    if let ToplevelPointerInteraction::Resize {
                        last_window_size: lw,
                        ..
                    } = slot
                    {
                        *lw = last_window_size;
                    }
                }
                return true;
            }
            None => {}
        }
        false
    }

    fn process_toplevel_pointer_button(&mut self, button: FlowMouseButton, state: FlowKeyState) {
        if !matches!(button, FlowMouseButton::Left) {
            return;
        }
        if !matches!(state, FlowKeyState::Released) {
            return;
        }
        let Some(active) = self.toplevel_pointer.take() else {
            return;
        };
        match active {
            ToplevelPointerInteraction::Resize {
                window_id,
                edges,
                initial_rect,
                last_window_size,
                ..
            } => {
                if let Some(w) = self.window(window_id) {
                    if let Some(tl) = w.window.toplevel() {
                        tl.with_pending_state(|st| {
                            st.states.unset(xdg_toplevel::State::Resizing);
                            st.size = Some(last_window_size);
                        });
                        tl.send_pending_configure();
                        ResizeSurfaceState::set_waiting_for_commit(
                            tl.wl_surface(),
                            edges,
                            initial_rect,
                        );
                    }
                }
            }
            ToplevelPointerInteraction::Move { window_id, .. } => {
                if let Some(w) = self.window(window_id) {
                    if let Some(x11) = w.window.x11_surface() {
                        if let Some(bbox) = self.space.element_bbox(&w.window) {
                            let _ =
                                x11.configure(Rectangle::from_loc_and_size(bbox.loc, bbox.size));
                            let window = w.window.clone();
                            self.map_window_bbox_location(window, bbox.loc, false);
                        }
                    }
                }
            }
        }
        self.forward_pointer_to_clients(self.pointer_pos);
        self.mark_focused_output_full_damage(DamageSource::Unknown);
    }

    pub fn handle_input(&mut self, event: FlowInputEvent) {
        if self.debug.show_input_events {
            focaldesk_logging::logging::flog(FLogLevel::Debug, format!("input event: {event:?}"));
        }

        if self.lock_screen.active {
            match event {
                FlowInputEvent::Key { keycode, state, .. } => {
                    self.handle_lock_key_event(keycode, state);
                }
                FlowInputEvent::PointerMoved { position } => {
                    self.input.pointer_pos = position;
                    self.pointer_pos = position;
                    self.cursor_manager.move_to(position.x, position.y);
                    self.clear_client_pointer_focus(position);
                }
                FlowInputEvent::PointerButton {
                    button,
                    state,
                    position,
                } => {
                    self.input.pointer_pos = position;
                    self.pointer_pos = position;
                    self.cursor_manager.move_to(position.x, position.y);
                    self.clear_client_pointer_focus(position);
                    if matches!(
                        (button, state),
                        (FlowMouseButton::Left, FlowKeyState::Pressed)
                    ) {
                        self.toggle_lock_password_visibility_at(position);
                    }
                }
                FlowInputEvent::PointerScroll { position, .. } => {
                    self.input.pointer_pos = position;
                    self.pointer_pos = position;
                    self.cursor_manager.move_to(position.x, position.y);
                    self.clear_client_pointer_focus(position);
                }
                FlowInputEvent::PointerEntered => {
                    self.cursor_manager.set_visible(true);
                }
                FlowInputEvent::PointerLeft => {
                    self.cursor_manager.set_visible(false);
                }
                FlowInputEvent::Resized {
                    output_id,
                    width,
                    height,
                    scale_factor,
                } => {
                    let size = Size::<i32, Physical>::from((width as i32, height as i32));
                    self.update_output_size(output_id, size, scale_factor);
                }
                FlowInputEvent::CloseRequested => {
                    self.running = false;
                }
            }

            self.pending_compositor_move = None;
            self.pending_xdg_move = None;
            self.toplevel_pointer = None;
            self.input.pointer_left_down = false;
            self.mark_all_outputs_full_damage(DamageSource::Unknown);
            return;
        }

        match event {
            FlowInputEvent::Key { keycode, state, .. } => {
                if matches!(state, FlowKeyState::Pressed)
                    && keycode == 1
                    && self.render.egui.has_open_panels()
                {
                    self.render.egui.close_all_panels();
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                if self.handle_egui_input(&event) {
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                // Modal dialogs intercept inside `keyboard.input` (still updates XKB / modifier state).
                self.handle_key_event(keycode, state);
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }

            FlowInputEvent::PointerMoved { position } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;
                if let Some(id) = self.output_under_pointer(position) {
                    self.set_focused_output(id);
                }
                let cursor_owner_damage = self.update_cursor_owner_damage();
                let stale_cursor_damage = self.clear_stale_software_cursor_damage();
                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                    if self.render.egui.wants_pointer_input() {
                        self.clear_client_pointer_focus(self.pointer_pos);
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                        return;
                    }
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                self.cursor_manager.move_to(position.x, position.y);
                const DRAG_THRESHOLD_SQ: f64 = 5.0 * 5.0;
                if self.input.pointer_left_down {
                    if let Some((id, start)) = self.pending_xdg_move {
                        let d = position - start;
                        if d.x * d.x + d.y * d.y >= DRAG_THRESHOLD_SQ {
                            self.pending_xdg_move = None;
                            self.request_move(id);
                        }
                    }
                    if let Some((id, start)) = self.pending_compositor_move {
                        let d = position - start;
                        if d.x * d.x + d.y * d.y >= DRAG_THRESHOLD_SQ {
                            self.pending_compositor_move = None;
                            self.try_begin_compositor_move(id);
                        }
                    }
                } else {
                    self.pending_xdg_move = None;
                    if matches!(
                        self.toplevel_pointer,
                        Some(ToplevelPointerInteraction::Move { .. })
                    ) {
                        self.toplevel_pointer = None;
                        self.forward_pointer_to_clients(self.pointer_pos);
                    }
                }
                let precise_toplevel_damage = self.process_toplevel_pointer_motion(position);
                let precise_hover_damage = self.update_ui_hover_for_output(self.focused_output);
                self.update_pointer_cursor(position);
                if !self.compositor_pointer_grab_active() {
                    self.forward_pointer_to_clients(position);
                }
                let precise_cursor_damage =
                    self.software_cursor_damage_pending_for_output(self.focused_output);
                if !precise_toplevel_damage
                    && !precise_hover_damage
                    && !precise_cursor_damage
                    && !stale_cursor_damage
                    && !cursor_owner_damage
                {
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                }
            }

            FlowInputEvent::PointerButton {
                button,
                state,
                position,
                ..
            } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;

                if let Some(id) = self.output_under_pointer(position) {
                    self.set_focused_output(id);
                }
                let cursor_owner_damage = self.update_cursor_owner_damage();
                let stale_cursor_damage = self.clear_stale_software_cursor_damage();

                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }

                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }

                if matches!(button, FlowMouseButton::Left)
                    && matches!(state, FlowKeyState::Released)
                    && self.suppress_next_left_release
                {
                    self.suppress_next_left_release = false;
                    self.input.pointer_left_down = false;
                    self.pending_compositor_move = None;
                    self.pending_xdg_move = None;
                    self.update_pointer_cursor(position);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }

                if matches!(button, FlowMouseButton::Left)
                    && self.peek_ui_action_at_pointer().is_some()
                {
                    self.clear_client_pointer_focus(position);
                    let mut damaged_precisely = false;
                    match state {
                        FlowKeyState::Pressed => {
                            self.input.pointer_left_down = true;
                            let pressed_element = self
                                .ui_element_at_pointer_for_output(self.focused_output)
                                .map(|el| (el.id, el.kind));
                            self.ui.pressed = pressed_element.map(|(id, _)| id);
                            if pressed_element
                                .is_some_and(|(_, kind)| kind == UiElementKind::SidebarButton)
                            {
                                self.play_ui_sound(UiSound::Select);
                            }
                            damaged_precisely = match pressed_element {
                                Some((_, UiElementKind::SidebarButton)) => {
                                    self.trigger_sidebar_pulse_at_pointer(self.focused_output)
                                }
                                Some((_, UiElementKind::TopbarIndicator)) => {
                                    self.trigger_topbar_pulse_at_pointer(self.focused_output)
                                }
                                Some((_, UiElementKind::Clock)) => {
                                    self.trigger_clock_pulse_at_pointer(self.focused_output)
                                }
                                _ => false,
                            };
                            let _ = self.click_ui_at_pointer();
                        }
                        FlowKeyState::Released => {
                            self.input.pointer_left_down = false;
                            self.ui.pressed = None;
                            self.pending_compositor_move = None;
                        }
                    }
                    if !damaged_precisely {
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                    }
                    return;
                }

                if matches!(button, FlowMouseButton::Left)
                    && matches!(state, FlowKeyState::Released)
                    && self.ui.pressed.is_some()
                {
                    self.input.pointer_left_down = false;
                    self.ui.pressed = None;
                    self.pending_compositor_move = None;
                    self.clear_client_pointer_focus(position);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }

                if self.render.egui.has_open_panels()
                    && matches!(button, FlowMouseButton::Left)
                    && matches!(state, FlowKeyState::Pressed | FlowKeyState::Released)
                {
                    self.process_egui_actions();
                    if matches!(state, FlowKeyState::Released)
                        && self.render.egui.wants_pointer_input()
                    {
                        self.clear_client_pointer_focus(self.pointer_pos);
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                        return;
                    }
                }

                if matches!(button, FlowMouseButton::Left) {
                    match state {
                        FlowKeyState::Pressed => {
                            self.input.pointer_left_down = true;
                        }
                        FlowKeyState::Released => {
                            self.input.pointer_left_down = false;
                            self.pending_compositor_move = None;
                            self.pending_xdg_move = None;
                        }
                    }
                }

                if let Some(id) = self.output_under_pointer(position) {
                    self.set_focused_output(id);
                }
                self.cursor_manager.move_to(position.x, position.y);
                let precise_toplevel_damage = self.process_toplevel_pointer_motion(position);
                let precise_hover_damage = self.update_ui_hover_for_output(self.focused_output);
                if matches!(state, FlowKeyState::Pressed) {
                    if matches!(button, FlowMouseButton::Left)
                        && self.pointer_on_chrome_host_drag_region(position)
                    {
                        self.host_window_drag_requested = true;
                        self.pending_compositor_move = None;
                    } else if matches!(button, FlowMouseButton::Left)
                        && self.handle_xwayland_titlebar_press(position)
                    {
                        self.clear_client_pointer_focus(position);
                        self.update_pointer_cursor(position);
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                        return;
                    } else {
                        self.focus_window_at(position);
                        if matches!(button, FlowMouseButton::Left)
                            && self.pointer_in_work_recess(position)
                        {
                            if let Some((id, edges)) = self.top_window_resize_edge_at(position) {
                                self.pending_compositor_move = None;
                                if let Ok(edge) = ResizeEdge::try_from(edges) {
                                    self.try_begin_compositor_resize(id, edge);
                                }
                            }
                        }
                    }
                }
                if self.compositor_pointer_grab_active() {
                    if matches!(button, FlowMouseButton::Left)
                        && matches!(state, FlowKeyState::Released)
                    {
                        self.process_toplevel_pointer_button(button, state);
                        self.forward_pointer_button(position, button, state);
                    }
                } else {
                    self.forward_pointer_to_clients(position);
                    self.forward_pointer_button(position, button, state);
                    self.process_toplevel_pointer_button(button, state);
                }
                self.update_pointer_cursor(position);
                let precise_cursor_damage =
                    self.software_cursor_damage_pending_for_output(self.focused_output);
                if !precise_toplevel_damage
                    && !precise_hover_damage
                    && !precise_cursor_damage
                    && !stale_cursor_damage
                    && !cursor_owner_damage
                {
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                }
            }

            FlowInputEvent::PointerScroll {
                position, delta, ..
            } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;
                if let Some(id) = self.output_under_pointer(position) {
                    self.set_focused_output(id);
                }
                self.update_cursor_owner_damage();
                self.clear_stale_software_cursor_damage();
                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                    if self.render.egui.wants_pointer_input() {
                        self.clear_client_pointer_focus(self.pointer_pos);
                        self.mark_focused_output_full_damage(DamageSource::Unknown);
                        return;
                    }
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_focused_output_full_damage(DamageSource::Unknown);
                    return;
                }
                self.cursor_manager.move_to(position.x, position.y);
                if !self.compositor_pointer_grab_active() {
                    self.forward_pointer_to_clients(position);
                    self.forward_pointer_scroll(delta);
                }
                self.mark_focused_output_full_damage(DamageSource::Unknown);
            }

            FlowInputEvent::PointerEntered => {
                self.cursor_manager.set_visible(true);
                self.update_cursor_owner_damage();
                self.mark_focused_output_full_damage(DamageSource::Cursor);
            }

            FlowInputEvent::PointerLeft => {
                let _ = self.handle_egui_input(&event);
                self.process_toplevel_pointer_button(FlowMouseButton::Left, FlowKeyState::Released);
                self.input.pointer_left_down = false;
                self.pending_compositor_move = None;
                self.pending_xdg_move = None;
                self.cursor_manager.set_visible(false);
                self.update_cursor_owner_damage();
                self.clear_all_software_cursor_damage();
                self.mark_focused_output_full_damage(DamageSource::Cursor);
            }

            FlowInputEvent::Resized {
                output_id,
                width,
                height,
                scale_factor,
            } => {
                //let id = OutputId(1);
                let size = Size::<i32, Physical>::from((width as i32, height as i32));
                self.update_output_size(output_id, size, scale_factor);

                // if let Some(output) = self.outputs.get_mut(&id) {
                //     output.scale_factor = scale_factor;
                //     output.scale = Scale::from((scale_factor, scale_factor));
                //     output.physical_size = size;
                //     let logical_w = (size.w as f64 / scale_factor).round() as i32;
                //     let logical_h = (size.h as f64 / scale_factor).round() as i32;
                //     output.logical_size = Size::<i32, Logical>::from((logical_w, logical_h));
                // }
            }

            FlowInputEvent::CloseRequested => {
                self.running = false;
            }
        }
    }

    fn handle_dialog_input(&mut self, event: &FlowInputEvent) -> bool {
        let Some(dialog_id) = self.active_dialog else {
            return false;
        };

        let Some(dialog) = self.dialogs.iter().find(|d| d.id == dialog_id) else {
            return false;
        };

        let Some(output) = self.outputs.get(&dialog.owner_output) else {
            return false;
        };

        let screen = Rectangle::from_loc_and_size((0, 0), output.logical_size);
        let layout = layout_dialog(dialog, screen);

        match event {
            FlowInputEvent::PointerButton { state, .. } => {
                if !matches!(state, FlowKeyState::Pressed) {
                    return true;
                }

                // `layout_dialog` uses output-local logical coordinates for the dialog owner.
                let Some(rel) = self.pointer_relative_to_output_logical(dialog.owner_output) else {
                    return true;
                };
                let px = rel.x.round() as i32;
                let py = rel.y.round() as i32;

                for (idx, rect) in &layout.button_rects {
                    if rect.contains((px, py)) {
                        let action = dialog.buttons[*idx].action;
                        self.handle_dialog_action(dialog.id, action);
                        return true;
                    }
                }

                let inside_panel = layout.bounds.contains((px, py));
                if !inside_panel {
                    // Modal dialogs: backdrop does not dismiss; only explicit buttons / Escape.
                    if dialog.dismissible && !dialog.modal {
                        self.close_dialog(dialog.id);
                    }
                }

                true
            }

            FlowInputEvent::PointerMoved { .. } => true,

            FlowInputEvent::PointerScroll { .. } => true,

            FlowInputEvent::Key { .. } => false,

            _ => false,
        }
    }

    /// Rectangle used to map winit/libinput absolute pointer coords into global logical space.
    /// Must match [`Space::output_geometry`] so hit testing and pointer forwarding agree (see anvil/smallvil).
    pub fn pointer_transform_rect_for_output(
        &self,
        output_id: OutputId,
    ) -> Rectangle<i32, Logical> {
        if let Some(output) = self.outputs.get(&output_id) {
            if let Some(geo) = self.space.output_geometry(&output.handle) {
                return geo;
            }
            return Rectangle::from_loc_and_size(output.logical_origin, output.logical_size);
        }
        Rectangle::from_loc_and_size((0, 0), (8192, 8192))
    }

    pub fn update_output_size(
        &mut self,
        output_id: OutputId,
        physical_size: Size<i32, Physical>,
        scale_factor: f64,
    ) {
        let mode = Mode {
            size: (physical_size.w, physical_size.h).into(),
            refresh: 60_000,
        };
        let scale_int = scale_factor.round().max(1.0) as i32;
        let logical_w = (physical_size.w as f64 / scale_factor).round() as i32;
        let logical_h = (physical_size.h as f64 / scale_factor).round() as i32;
        let logical_size = Size::<i32, Logical>::from((logical_w, logical_h));

        if let Some(output) = self.outputs.get_mut(&output_id) {
            output.scale_factor = scale_factor;
            output.scale = Scale::from((scale_factor, scale_factor));
            output.physical_size = physical_size;
            output.logical_size = logical_size;
            output.pending_damage = vec![Rectangle::from_loc_and_size((0, 0), physical_size)];
            output.last_sw_cursor_rect = None;

            output.handle.change_current_state(
                Some(mode),
                None,
                Some(OutputScaleSmithay::Custom {
                    advertised_integer: scale_int,
                    fractional: scale_factor,
                }),
                None,
            );
            output.handle.set_preferred(mode);
            self.space.map_output(&output.handle, output.logical_origin);
        }
    }

    pub fn needs_redraw(&self) -> bool {
        let now = Instant::now();
        self.render.redraw_all
            || self
                .outputs
                .values()
                .any(|output| !output.pending_damage.is_empty())
            || self.sidebar_pulse.is_some_and(|pulse| {
                now.saturating_duration_since(pulse.started_at) < SIDEBAR_PULSE_DURATION
            })
            || self.topbar_pulse.is_some_and(|pulse| {
                now.saturating_duration_since(pulse.started_at) < TOPBAR_PULSE_DURATION
            })
            || self.flow_field_pulse.is_some_and(|pulse| {
                now.saturating_duration_since(pulse.started_at) < FLOW_FIELD_PULSE_DURATION
            })
            || self.clock_pulse.is_some_and(|pulse| {
                now.saturating_duration_since(pulse.started_at) < CLOCK_PULSE_DURATION
            })
            || (self.lock_screen.active && self.lock_screen.pulse_frame(now).is_some())
    }

    pub fn output_has_pending_damage(&self, output_id: OutputId) -> bool {
        let now = Instant::now();
        self.outputs
            .get(&output_id)
            .map(|output| !output.pending_damage.is_empty())
            .unwrap_or(false)
            || self.output_has_active_sidebar_pulse(output_id, now)
            || self.output_has_active_topbar_pulse(output_id, now)
            || self.output_has_active_flow_field_pulse(output_id, now)
            || self.output_has_active_clock_pulse(output_id, now)
            || (self.lock_screen.active && self.lock_screen.pulse_frame(now).is_some())
    }

    pub fn clear_repaint_request(&mut self) {
        self.render.redraw_all = false;
        for output in self.outputs.values_mut() {
            output.pending_damage.clear();
        }
        self.damage_source_counts = DamageSourceCounts::default();
    }

    pub fn mark_redraw(&mut self) {
        self.render.redraw_all = true;
    }

    pub fn mark_output_full_damage(&mut self, output_id: OutputId, source: DamageSource) {
        let Some(output) = self.outputs.get(&output_id) else {
            return;
        };
        self.mark_output_damage_source(
            output_id,
            Rectangle::from_loc_and_size((0, 0), output.physical_size),
            source,
        );
    }

    pub fn mark_focused_output_full_damage(&mut self, source: DamageSource) {
        self.mark_output_full_damage(self.focused_output, source);
    }

    pub fn mark_all_outputs_full_damage(&mut self, source: DamageSource) {
        let output_ids: Vec<OutputId> = self.outputs.keys().copied().collect();
        for output_id in output_ids {
            self.mark_output_full_damage(output_id, source);
        }
    }

    fn mark_all_outputs_clock_damage(&mut self, source: DamageSource) {
        let damage: Vec<(OutputId, Rectangle<i32, Logical>)> = self
            .outputs
            .iter()
            .map(|(output_id, output)| {
                let layout = build_chrome_layout(
                    output.logical_size,
                    self.chrome.metrics.topbar_h,
                    self.chrome.metrics.sidebar_w,
                );
                (*output_id, layout.topbar.clock_well)
            })
            .collect();

        for (output_id, rect) in damage {
            self.mark_output_logical_damage(output_id, rect, 2, source);
        }
    }

    fn mark_all_outputs_chrome_controls_damage(&mut self, source: DamageSource) {
        let damage: Vec<(OutputId, Rectangle<i32, Logical>)> = self
            .outputs
            .iter()
            .flat_map(|(output_id, output)| {
                let layout = build_chrome_layout(
                    output.logical_size,
                    self.chrome.metrics.topbar_h,
                    self.chrome.metrics.sidebar_w,
                );
                [
                    (*output_id, layout.sidebar.outer),
                    (*output_id, layout.topbar.inner),
                    (*output_id, layout.topbar.flow_field),
                ]
            })
            .collect();

        for (output_id, rect) in damage {
            self.mark_output_logical_damage(output_id, rect, 2, source);
        }
    }

    pub fn mark_output_damage(&mut self, output_id: OutputId, rect: Rectangle<i32, Physical>) {
        self.mark_output_damage_source(output_id, rect, DamageSource::Unknown);
    }

    pub fn mark_output_damage_source(
        &mut self,
        output_id: OutputId,
        rect: Rectangle<i32, Physical>,
        source: DamageSource,
    ) {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }

        if let Some(output) = self.outputs.get_mut(&output_id) {
            let bounds = Rectangle::from_loc_and_size((0, 0), output.physical_size);
            if let Some(clipped) = rect.intersection(bounds) {
                output.pending_damage.push(clipped);
                self.damage_source_counts.record(source);
            }
        }
    }

    pub fn record_damage_source(&mut self, source: DamageSource) {
        self.damage_source_counts.record(source);
    }

    pub fn damage_debug_enabled(&self) -> bool {
        self.damage_debug_enabled
    }

    pub fn log_damage_frame(
        &self,
        output_id: OutputId,
        pre_rects: usize,
        post_rects: usize,
        pre_area_percent: i64,
        post_area_percent: i64,
        full_damage: bool,
        redraw_all: bool,
    ) {
        if !self.damage_debug_enabled || !self.render.frame_no.is_multiple_of(120) {
            return;
        }

        let c = self.damage_source_counts;
        flog(format!(
            "damage output={:?} frame={} rects={}->{} area={}%%->{}%% full={} redraw_all={} src(move={}, resize={}, cursor={}, hover={}, commit={}, full_fallback={}, unknown={})",
            output_id,
            self.render.frame_no,
            pre_rects,
            post_rects,
            pre_area_percent,
            post_area_percent,
            full_damage,
            redraw_all,
            c.window_move,
            c.window_resize,
            c.cursor,
            c.hover,
            c.commit_bbox,
            c.full_redraw_fallback,
            c.unknown
        ));
    }

    fn expand_physical_rect(
        rect: Rectangle<i32, Physical>,
        margin: i32,
    ) -> Rectangle<i32, Physical> {
        Rectangle::from_loc_and_size(
            (rect.loc.x - margin, rect.loc.y - margin),
            (rect.size.w + margin * 2, rect.size.h + margin * 2),
        )
    }

    /*
        pub fn handle_resize(
            &mut self,
            size: smithay::utils::Size<i32, smithay::utils::Physical>,
            output_id: OutputId,
        ) {
            let id = output_id;

            if let Some(output) = self.outputs.get_mut(&id) {
                    let logical_w = (size.w as f64 / output.scale_factor).round() as i32;
                    let logical_h = (size.h as f64 / output.scale_factor).round() as i32;

                    output.physical_size = size;
                    output.logical_size = Size::<i32, Logical>::from((logical_w, logical_h));
                    //output.scale = Scale::from((scale_factor, scale_factor));
                }

            // For now: queue output damage.
            // Later: update layout/output metrics properly.
        }
    */
    /// Wire the nested compositor's single `wl_output` ([`Output`] with [`Output::create_global`])
    /// into desktop state. Must be the **same** [`Output`] advertised to clients so
    /// `ext-output-image-capture-source-v1` and [`crate::core::portal::output_id_for_session`]
    /// resolve to this entry (required for OBS / `xdg-desktop-portal-wlr`).
    pub fn set_output_from_nested(
        &mut self,
        handle: Output,
        size: Size<i32, Physical>,
        scale: f64,
    ) {
        let id = OutputId(1);

        let logical_w = (size.w as f64 / scale).round() as i32;
        let logical_h = (size.h as f64 / scale).round() as i32;

        let mode = Mode {
            size: (size.w, size.h).into(),
            refresh: 60_000,
        };
        let scale_int = scale.round().max(1.0) as i32;
        handle.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(OutputScaleSmithay::Custom {
                advertised_integer: scale_int,
                fractional: scale,
            }),
            Some((0, 0).into()),
        );
        handle.set_preferred(mode);

        if let Some(output) = self.outputs.get_mut(&id) {
            output.handle = handle;
            output.physical_size = size;
            output.logical_size = Size::<i32, Logical>::from((logical_w, logical_h));
            output.scale_factor = scale;
            output.scale = Scale::from((scale, scale));
        } else {
            self.space
                .map_output(&handle, Point::<i32, Logical>::from((0, 0)));
            self.outputs.insert(
                id,
                OutputState {
                    handle,
                    physical_size: size,
                    logical_size: Size::<i32, Logical>::from((logical_w, logical_h)),
                    logical_origin: Point::<i32, Logical>::from((0, 0)),
                    scale_factor: scale,
                    scale: Scale::from((scale, scale)),
                    hdr_supported: false,
                    hdr_requested: false,
                    hdr_enabled: false,
                    edid_hdr_max_luminance_nits: None,
                    edid_hdr_max_fall_nits: None,
                    active_workspace: WorkspaceId(1),
                    pending_damage: vec![Rectangle::from_loc_and_size((0, 0), size)],
                    last_sw_cursor_rect: None,
                    color_description: crate::core::color::default_output_color_description(),
                    icc_profile: None,
                    output_icc_lut: None,
                    icc_lut_fallback_active: false,
                    monitor_make: String::new(),
                    monitor_model: String::new(),
                    monitor_serial: String::new(),
                    monitor_edid: None,
                },
            );
        }

        self.cursor_manager
            .set_base_size_and_scale(24, scale as f32);
    }

    pub fn insert_nested_output(
        &mut self,
        _output: Output,
        _size: Size<i32, Physical>,
        _scale: f64,
    ) {
    }

    pub fn tick_layout(&mut self) {
        self.popups.cleanup();
        self.refresh_ai_flow_mode();
    }

    /// Update output enter/leave and refresh mapped client surfaces. Call before flushing Wayland clients.
    pub fn refresh_space(&mut self) {
        self.space.refresh();
    }

    pub fn output_color_description(&self, output_id: focaldesk_types::OutputId) -> ColorDescription {
        self.outputs
            .get(&output_id)
            .map(|output| output.color_description)
            .unwrap_or_else(crate::core::color::default_output_color_description)
    }

    pub fn output_color_description_for(&self, output: &smithay::output::Output) -> ColorDescription {
        self.outputs
            .values()
            .find(|state| &state.handle == output)
            .map(|state| state.color_description)
            .unwrap_or_else(crate::core::color::default_output_color_description)
    }

    pub fn output_icc_profile_for(&self, output: &smithay::output::Output) -> Option<Vec<u8>> {
        self.outputs
            .values()
            .find(|state| &state.handle == output)
            .and_then(|state| state.icc_profile.clone())
    }

    pub fn set_output_monitor_identity(
        &mut self,
        output_id: focaldesk_types::OutputId,
        make: String,
        model: String,
        serial: String,
        edid: Option<Vec<u8>>,
    ) {
        if let Some(output) = self.outputs.get_mut(&output_id) {
            output.monitor_make = make;
            output.monitor_model = model;
            output.monitor_serial = serial;
            output.monitor_edid = edid;
        }
    }

    pub fn output_icc_lut_for(
        &self,
        output_id: focaldesk_types::OutputId,
    ) -> Option<&crate::core::icc_lut::OutputIccLut> {
        self.outputs
            .get(&output_id)
            .and_then(|output| output.output_icc_lut.as_ref())
    }

    pub fn set_output_color(
        &mut self,
        output_id: focaldesk_types::OutputId,
        description: ColorDescription,
        icc_profile: Option<Vec<u8>>,
        output_icc_lut: Option<crate::core::icc_lut::OutputIccLut>,
    ) {
        let output_lut = output_icc_lut.or_else(|| {
            let Some(bytes) = icc_profile.as_ref() else {
                return None;
            };
            match crate::core::icc_lut::build_srgb_to_device_lut(bytes) {
                Ok(lut) => Some(lut),
                Err(err) => {
                    flog_warn!(
                        "output color: failed to bake ICC LUT for output {:?}: {:?}",
                        output_id,
                        err
                    );
                    None
                }
            }
        });
        if let Some(output) = self.outputs.get_mut(&output_id) {
            output.color_description = description;
            output.icc_profile = icc_profile;
            output.output_icc_lut = output_lut;
            output.icc_lut_fallback_active = false;
            flog_warn!(
                "output color: id={output_id:?} serial={} primaries={:?} transfer={:?} icc={} lut={}",
                output.monitor_serial,
                description.primaries,
                description.transfer,
                output.icc_profile.as_ref().map(|p| p.len()).unwrap_or(0),
                output.output_icc_lut.as_ref().map(|l| l.rgb.len()).unwrap_or(0),
            );
        }
        self.notify_runtime_display_status_changes();
    }

    pub fn refresh_surface_color(&mut self, surface: &WlSurface) {
        let force_linear = force_linear_surfaces();
        let color = with_states(surface, |states| {
            crate::core::color::effective_surface_render_state(states, force_linear)
        });
        let id = Id::from_wayland_resource(surface);
        let previous = self.surface_colors.get(&id).copied();
        if previous != Some(color) {
            focaldesk_logging::flog(format!(
                "surface color applied: surface={id:?} transfer={:?} primaries={:?}",
                color.description.transfer,
                color.description.primaries
            ));
        }
        self.surface_colors.insert(id, color);
    }

    pub fn set_surface_color_description(
        &mut self,
        surface: &WlSurface,
        description: ColorDescription,
    ) {
        with_states(surface, |states| {
            states
                .cached_state
                .get::<SurfaceColorState>()
                .pending()
                .description = Some(description);
        });
        self.refresh_surface_color(surface);
    }

    /// Import committed buffers for mapped windows on this output before building render elements.
    pub fn import_mapped_surfaces_for_output<R>(
        &self,
        renderer: &mut R,
        origin: Point<i32, Logical>,
        logical_size: Size<i32, Logical>,
    ) where
        R: smithay::backend::renderer::Renderer + smithay::backend::renderer::ImportAll,
        R::TextureId: 'static,
    {
        use smithay::backend::allocator::Buffer as SmithayBuffer;
        use smithay::backend::renderer::buffer_type;
        use smithay::backend::renderer::utils::import_surface_tree;
        use smithay::utils::Rectangle;
        use smithay::wayland::compositor::{BufferAssignment, SurfaceAttributes};
        use smithay::wayland::dmabuf::get_dmabuf;

        let output_rect = Rectangle::from_loc_and_size(origin, logical_size);

        for window in self.space.elements() {
            let Some(global_bbox) = self.global_window_bbox(window) else {
                continue;
            };
            if !global_bbox.overlaps(output_rect) {
                continue;
            }
            let Some(surface) = window.wl_surface() else {
                continue;
            };
            if let Err(err) = import_surface_tree(renderer, &surface) {
                let buffer_info = with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer
                        .as_ref()
                        .map(|assignment| match assignment {
                            BufferAssignment::Removed => "removed".to_string(),
                            BufferAssignment::NewBuffer(buffer) => {
                                let kind = format!("{:?}", buffer_type(buffer));
                                if let Ok(dmabuf) = get_dmabuf(buffer) {
                                    format!(
                                        "{kind} format={:?} planes={} y_inverted={}",
                                        SmithayBuffer::format(dmabuf),
                                        dmabuf.num_planes(),
                                        dmabuf.y_inverted()
                                    )
                                } else {
                                    kind
                                }
                            }
                        })
                        .unwrap_or_else(|| "unchanged".to_string())
                });
                focaldesk_logging::flog(format!(
                    "frame surface import failed: {err:?}; root_buffer={buffer_info}"
                ));
            }
        }
    }

    pub fn send_frame_callbacks(&mut self, _millis: u32) {
        let time = Duration::from_millis(_millis.into());
        let fallback_output = self
            .outputs
            .get(&self.focused_output)
            .or_else(|| self.outputs.get(&self.primary_output))
            .map(|output| output.handle.clone());

        for window in self.space.elements() {
            let mut outputs = self.space.outputs_for_element(window);
            if outputs.is_empty() {
                if let Some(output) = fallback_output.clone() {
                    outputs.push(output);
                }
            }
            for output in outputs {
                window.send_frame(&output, time, None, |_, _| Some(output.clone()));
            }
        }
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut ManagedWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn window(&self, id: WindowId) -> Option<&ManagedWindow> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn window_id_for_wl_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows.iter().find_map(|w| {
            w.wl_surface()
                .as_ref()
                .and_then(|wl| if &**wl == surface { Some(w.id) } else { None })
        })
    }

    pub fn window_id_for_toplevel(&self, surface: &ToplevelSurface) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|w| w.matches_toplevel(surface))
            .map(|w| w.id)
    }

    pub fn lookup_window_id_for_surface(&self, surface: &ToplevelSurface) -> Option<WindowId> {
        self.window_id_for_toplevel(surface)
    }

    pub fn queue_deferred_move(&mut self, id: WindowId) {
        self.pending_xdg_move = Some((id, self.pointer_pos));
    }

    pub fn queue_xdg_move_request(&mut self, id: WindowId) {
        self.queue_deferred_move(id);
    }

    pub fn request_move(&mut self, id: WindowId) {
        if self.toplevel_pointer.is_some() {
            return;
        }
        let Some(w) = self.window(id) else {
            return;
        };
        let Some(loc) = self.space.element_location(&w.window) else {
            return;
        };
        self.clear_client_pointer_focus(self.pointer_pos);
        self.toplevel_pointer = Some(ToplevelPointerInteraction::Move {
            window_id: id,
            pointer_start: self.pointer_pos,
            initial_location: loc,
        });
        if let Some(window) = self.window_mut(id) {
            window.pending_move = false;
        }
    }

    pub fn request_resize(&mut self, id: WindowId, edges: ResizeEdge) {
        if matches!(
            self.toplevel_pointer,
            Some(ToplevelPointerInteraction::Move { .. })
        ) {
            return;
        }
        let edges_m = ResizeEdgeMask::from(edges);
        let pointer_pos = self.pointer_pos;
        let Some((tl, initial_rect, last_window_size)) = self.window(id).and_then(|w| {
            let map_loc = self.space.element_location(&w.window)?;
            let geometry = w.window.geometry();
            let initial_rect = Rectangle::from_loc_and_size(map_loc, geometry.size);
            Some((w.window.toplevel()?.clone(), initial_rect, geometry.size))
        }) else {
            return;
        };

        self.clear_client_pointer_focus(pointer_pos);
        tl.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
        });
        tl.send_pending_configure();
        ResizeSurfaceState::set_resizing(tl.wl_surface(), edges_m, initial_rect);
        self.toplevel_pointer = Some(ToplevelPointerInteraction::Resize {
            window_id: id,
            edges: edges_m,
            pointer_start: pointer_pos,
            initial_rect,
            last_window_size,
        });
        if let Some(window) = self.window_mut(id) {
            window.pending_resize = None;
        }
    }

    pub(crate) fn set_window_maximized(&mut self, id: WindowId, maximized: bool) {
        let Some(idx) = self.windows.iter().position(|window| window.id == id) else {
            return;
        };

        if self.windows[idx].maximized == maximized {
            return;
        }

        let window = self.windows[idx].window.clone();
        let output_id = self
            .output_under_pointer(self.pointer_pos)
            .or(self.windows[idx].output)
            .unwrap_or(self.focused_output);
        let old_bbox = self.global_window_bbox(&window);

        if maximized {
            let Some(work) = self.work_recess_for_output(output_id) else {
                self.windows[idx].set_maximized(true);
                if let Some(rect) = old_bbox {
                    self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
                }
                return;
            };

            let restore_rect = self
                .space
                .element_bbox(&window)
                .or(self.windows[idx].float_rect)
                .unwrap_or_else(|| Rectangle::from_loc_and_size(work.loc, window.geometry().size));

            {
                let managed = &mut self.windows[idx];
                managed.restore_rect = Some(restore_rect);
                managed.set_maximized(true);
            }

            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Maximized);
                    state.size = Some(work.size);
                });
                toplevel.send_pending_configure();
            }

            if let Some(x11) = window.x11_surface() {
                let _ = x11.set_maximized(true);
                let _ = x11.configure(work);
            }

            self.map_window_bbox_location(window.clone(), work.loc, true);
        } else {
            let restore_rect = self.windows[idx].restore_rect.take().unwrap_or_else(|| {
                self.windows[idx].float_rect.unwrap_or_else(|| {
                    Rectangle::from_loc_and_size((100, 100), window.geometry().size)
                })
            });

            self.windows[idx].set_maximized(false);
            self.windows[idx].float_rect = Some(restore_rect);

            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Maximized);
                    state.size = Some(restore_rect.size);
                });
                toplevel.send_pending_configure();
            }

            if let Some(x11) = window.x11_surface() {
                let _ = x11.set_maximized(false);
                let _ = x11.configure(restore_rect);
            }

            self.map_window_bbox_location(window.clone(), restore_rect.loc, true);
        }

        self.space.refresh();
        if let Some(rect) = old_bbox {
            self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
        }
        if let Some(rect) = self.global_window_bbox(&window) {
            self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
        }
    }

    fn toggle_maximize(&mut self, id: WindowId) {
        let Some(maximized) = self.window(id).map(|window| window.maximized) else {
            return;
        };
        self.set_window_maximized(id, !maximized);
    }

    pub fn request_maximize(&mut self, id: WindowId) {
        self.set_window_maximized(id, true);
    }

    pub(crate) fn set_window_fullscreen(
        &mut self,
        id: WindowId,
        fullscreen: bool,
        requested_output: Option<wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let Some(idx) = self.windows.iter().position(|window| window.id == id) else {
            return;
        };

        if self.windows[idx].fullscreen == fullscreen {
            return;
        }

        let window = self.windows[idx].window.clone();
        let old_bbox = self.global_window_bbox(&window);
        let output_id = requested_output
            .as_ref()
            .and_then(|requested| {
                self.outputs
                    .iter()
                    .find_map(|(id, output)| output.handle.owns(requested).then_some(*id))
            })
            .or_else(|| self.output_under_pointer(self.pointer_pos))
            .or(self.windows[idx].output)
            .unwrap_or(self.focused_output);

        if fullscreen {
            let rect = self
                .outputs
                .get(&output_id)
                .and_then(|output| self.space.output_geometry(&output.handle))
                .or_else(|| {
                    self.outputs
                        .get(&self.primary_output)
                        .and_then(|output| self.space.output_geometry(&output.handle))
                });

            let Some(rect) = rect else {
                self.windows[idx].set_fullscreen(true);
                if let Some(rect) = old_bbox {
                    self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
                }
                return;
            };

            let restore_rect = self
                .space
                .element_bbox(&window)
                .or(self.windows[idx].float_rect)
                .unwrap_or_else(|| Rectangle::from_loc_and_size(rect.loc, window.geometry().size));

            {
                let managed = &mut self.windows[idx];
                managed.restore_rect = Some(restore_rect);
                managed.set_fullscreen(true);
                managed.set_maximized(false);
                managed.set_output(Some(output_id));
            }

            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Fullscreen);
                    state.states.unset(xdg_toplevel::State::Maximized);
                    state.size = Some(rect.size);
                    state.fullscreen_output = requested_output;
                });
                toplevel.send_pending_configure();
            }

            if let Some(x11) = window.x11_surface() {
                let _ = x11.set_fullscreen(true);
                let _ = x11.set_maximized(false);
                let _ = x11.configure(rect);
            }

            self.map_window_bbox_location(window.clone(), rect.loc, true);
        } else {
            let restore_rect = self.windows[idx].restore_rect.take().unwrap_or_else(|| {
                self.windows[idx].float_rect.unwrap_or_else(|| {
                    Rectangle::from_loc_and_size((100, 100), window.geometry().size)
                })
            });

            {
                let managed = &mut self.windows[idx];
                managed.set_fullscreen(false);
                managed.float_rect = Some(restore_rect);
            }

            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                    state.size = Some(restore_rect.size);
                    state.fullscreen_output = None;
                });
                toplevel.send_pending_configure();
            }

            if let Some(x11) = window.x11_surface() {
                let _ = x11.set_fullscreen(false);
                let _ = x11.configure(restore_rect);
            }

            self.map_window_bbox_location(window.clone(), restore_rect.loc, true);
        }

        self.space.refresh();
        if let Some(rect) = old_bbox {
            self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
        }
        if let Some(rect) = self.global_window_bbox(&window) {
            self.mark_window_bbox_damage_source(rect, DamageSource::WindowResize);
        }
    }

    pub fn request_fullscreen(
        &mut self,
        id: WindowId,
        requested_output: Option<wayland_server::protocol::wl_output::WlOutput>,
    ) {
        self.set_window_fullscreen(id, true, requested_output);
    }

    pub fn request_unfullscreen(&mut self, id: WindowId) {
        self.set_window_fullscreen(id, false, None);
    }

    pub fn prepare_cursor_for_frame(
        &mut self,
        renderer: &mut GlesRenderer,
        output_id: OutputId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(desk_output) = self.outputs.get(&output_id) else {
            return Ok(());
        };
        let output_scale = desk_output.scale;
        let output_scale_factor = desk_output.scale_factor;
        let previous_cursor_rect = desk_output.last_sw_cursor_rect;
        self.cursor_manager
            .set_base_size_and_scale(24, output_scale_factor as f32);
        self.cursor_manager
            .move_to(self.pointer_pos.x, self.pointer_pos.y);

        if !self.cursor_manager.visible() {
            self.render.clear_sw_cursor_texture();
            self.render.sw_cursor_dst_rect = None;
            if let Some(output) = self.outputs.get_mut(&output_id) {
                output.last_sw_cursor_rect = None;
            }
            if let Some(old_rect) = previous_cursor_rect {
                self.mark_output_damage_source(
                    output_id,
                    Self::expand_physical_rect(old_rect, 4),
                    DamageSource::Cursor,
                );
            }
            return Ok(());
        }

        self.render
            .upload_cursor_texture_for_desktop(renderer, &mut self.cursor_manager)?;

        let need_sw = self.output_owns_cursor(output_id)
            && self.cursor_manager.software_cursor_needed()
            && !self.drm_try_pass_cursor_this_frame;
        if need_sw {
            let rel = self
                .pointer_relative_to_output_logical(output_id)
                .unwrap_or(self.pointer_pos);
            let phys: Point<i32, Physical> =
                rel.to_physical_precise_round::<f64, i32>(output_scale);
            let (hx, hy) = self.render.sw_cursor_hotspot;
            let (tw, th) = self.render.sw_cursor_tex_size;
            let cursor_rect =
                Rectangle::<i32, Physical>::from_loc_and_size((phys.x - hx, phys.y - hy), (tw, th));
            self.render.sw_cursor_dst_rect = Some((
                cursor_rect.loc.x,
                cursor_rect.loc.y,
                cursor_rect.size.w,
                cursor_rect.size.h,
            ));

            if previous_cursor_rect != Some(cursor_rect) {
                if let Some(old_rect) = previous_cursor_rect {
                    self.mark_output_damage_source(
                        output_id,
                        Self::expand_physical_rect(old_rect, 4),
                        DamageSource::Cursor,
                    );
                }
                self.mark_output_damage_source(
                    output_id,
                    Self::expand_physical_rect(cursor_rect, 4),
                    DamageSource::Cursor,
                );
            }
            if let Some(output) = self.outputs.get_mut(&output_id) {
                output.last_sw_cursor_rect = Some(cursor_rect);
            }
        } else {
            self.render.sw_cursor_dst_rect = None;
            if let Some(output) = self.outputs.get_mut(&output_id) {
                output.last_sw_cursor_rect = None;
            }
            if let Some(old_rect) = previous_cursor_rect {
                self.mark_output_damage_source(
                    output_id,
                    Self::expand_physical_rect(old_rect, 4),
                    DamageSource::Cursor,
                );
            }
        }
        Ok(())
    }

    /// After [`smithay::backend::drm::DrmOutput::render_frame`], reconcile whether the cursor was skipped.
    pub fn update_cursor_policy_after_drm_present(
        &mut self,
        states: &RenderElementStates,
        cursor_on_hw_plane: bool,
    ) {
        if cursor_on_hw_plane {
            self.cursor_manager.set_hardware_cursor_ready(true);
            return;
        }

        if self.drm_submit_hw_cursor {
            if let Some(s) = states.element_render_state(self.drm_cursor_render_id.clone()) {
                if matches!(
                    s.presentation_state,
                    RenderElementPresentationState::Skipped
                ) {
                    self.drm_submit_hw_cursor = false;
                    self.cursor_manager.set_hardware_cursor_ready(false);
                    return;
                }
                if matches!(
                    s.presentation_state,
                    RenderElementPresentationState::Rendering { .. }
                        | RenderElementPresentationState::ZeroCopy
                ) && s.visible_area > 0
                {
                    self.cursor_manager.set_hardware_cursor_ready(true);
                }
            }
        }
    }
}

fn chrome_profile_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        })
        .join("focaldesk")
        .join("chrome-profile")
}

fn clear_stale_chrome_singleton(profile: &Path) {
    let lock = profile.join("SingletonLock");
    let Ok(target) = std::fs::read_link(&lock) else {
        return;
    };

    let Some(pid) = target
        .to_string_lossy()
        .rsplit('-')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return;
    };

    if PathBuf::from(format!("/proc/{pid}")).exists() {
        return;
    }

    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let path = profile.join(name);
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                flog(format!(
                    "failed to remove stale Chrome singleton {}: {err}",
                    path.display()
                ));
            }
        }
    }
    flog(format!(
        "removed stale Chrome profile singleton for pid {pid}"
    ));
}

fn ai_flow_mode_from_status(status: &AiDaemonStatus) -> AiFlowMode {
    if status.active_requests > 0 {
        AiFlowMode::Thinking
    } else if status.provider_count == 0 {
        AiFlowMode::Error
    } else {
        AiFlowMode::Idle
    }
}

fn focaldesk_files_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("focaldesk-files")))
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "focaldesk-files".to_string())
}

fn focaldesk_settings_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("focaldesk-settings"))
        })
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "focaldesk-settings".to_string())
}

fn focaldesk_ai_console_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("focaldesk-ai-console"))
        })
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "focaldesk-ai-console".to_string())
}

fn configure_chrome_command(command: &mut Command, use_x11: bool) {
    let profile = chrome_profile_dir();
    clear_stale_chrome_singleton(&profile);
    let ozone_platform = if use_x11 { "x11" } else { "wayland" };
    command
        .arg(format!("--ozone-platform={ozone_platform}"))
        .arg("--disable-features=Vulkan")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--new-window");
}

fn chrome_launch_trace_path() -> PathBuf {
    PathBuf::from("/tmp/focaldesk-chrome.trace")
}

fn chrome_launch_trace(msg: impl AsRef<str>) {
    let line = format!("[chrome-launch] {}", msg.as_ref());
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(chrome_launch_trace_path())
    {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

fn chrome_launch_note(msg: impl AsRef<str>) {
    let message = msg.as_ref().to_string();
    chrome_launch_trace(&message);
    flog_warn!("[chrome-launch] {message}");
}

fn open_session_log_file() -> Option<std::fs::File> {
    for path in log_file_path_candidates() {
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            return Some(file);
        }
    }

    None
}

fn chrome_exec_fallbacks(app_name: &str) -> Vec<String> {
    let executable = app_name.rsplit('/').next().unwrap_or(app_name);
    let mut candidates = vec![
        "google-chrome".to_string(),
        "google-chrome-stable".to_string(),
        "google-chrome-beta".to_string(),
        "google-chrome-unstable".to_string(),
        "chromium".to_string(),
        "chromium-browser".to_string(),
    ];

    if let Some(idx) = candidates.iter().position(|name| name == executable) {
        if idx != 0 {
            let preferred = candidates.remove(idx);
            candidates.insert(0, preferred);
        }
    } else {
        candidates.insert(0, executable.to_string());
    }

    candidates
}

fn is_chrome_like(app_name: &str) -> bool {
    let executable = app_name.rsplit('/').next().unwrap_or(app_name);
    matches!(
        executable,
        "google-chrome"
            | "google-chrome-stable"
            | "google-chrome-beta"
            | "google-chrome-unstable"
            | "chrome"
            | "chromium"
            | "chromium-browser"
    )
}

pub(crate) fn is_browser_like(app_name: &str) -> bool {
    let executable = app_name.rsplit('/').next().unwrap_or(app_name);
    matches!(
        executable,
        "firefox"
            | "firefox-esr"
            | "firefox-beta"
            | "firefox-developer-edition"
            | "mozilla-firefox"
            | "google-chrome"
            | "google-chrome-stable"
            | "google-chrome-beta"
            | "google-chrome-unstable"
            | "chrome"
            | "chromium"
            | "chromium-browser"
            | "microsoft-edge"
            | "microsoft-edge-stable"
            | "microsoft-edge-beta"
            | "microsoft-edge-dev"
            | "edge"
            | "edge-beta"
            | "edge-dev"
            | "brave"
            | "brave-browser"
            | "librewolf"
            | "waterfox"
    )
}

fn is_cursor_like(app_name: &str) -> bool {
    let executable = app_name.rsplit('/').next().unwrap_or(app_name);
    matches!(executable, "cursor" | "cursor-bin")
}

fn is_obs_like(app_name: &str) -> bool {
    let executable = app_name.rsplit('/').next().unwrap_or(app_name);
    matches!(executable, "obs" | "obs-studio" | "com.obsproject.Studio")
}

#[cfg(test)]
mod tests {
    use super::{clamp_rect_to_bounds, is_browser_like};
    use smithay::utils::Rectangle;

    #[test]
    fn browser_like_matches_common_browser_executables() {
        assert!(is_browser_like("google-chrome"));
        assert!(is_browser_like("chrome"));
        assert!(is_browser_like("firefox"));
        assert!(is_browser_like("microsoft-edge"));
        assert!(is_browser_like("brave-browser"));
        assert!(!is_browser_like("alacritty"));
        assert!(!is_browser_like("cursor"));
    }

    #[test]
    fn clamp_rect_to_bounds_keeps_rect_inside_area() {
        let rect = Rectangle::from_loc_and_size((90, 90), (40, 40));
        let bounds = Rectangle::from_loc_and_size((10, 20), (100, 80));

        let clamped = clamp_rect_to_bounds(rect, bounds);

        assert_eq!(clamped.loc, (70, 60).into());
        assert_eq!(clamped.size, (40, 40).into());
    }

    #[test]
    fn clamp_rect_to_bounds_shrinks_oversized_rect() {
        let rect = Rectangle::from_loc_and_size((0, 0), (200, 200));
        let bounds = Rectangle::from_loc_and_size((10, 20), (100, 80));

        let clamped = clamp_rect_to_bounds(rect, bounds);

        assert_eq!(clamped.loc, (10, 20).into());
        assert_eq!(clamped.size, (100, 80).into());
    }
}

fn home_videos_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Videos"))
}

fn obs_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("obs-studio")
}

fn configure_obs_recording_dir() {
    let Some(videos_dir) = home_videos_dir() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(&videos_dir) {
        flog(format!(
            "failed to create OBS recording directory {}: {err}",
            videos_dir.display()
        ));
        return;
    }

    let profile_root = obs_config_dir().join("basic").join("profiles");
    let Ok(entries) = std::fs::read_dir(&profile_root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path().join("basic.ini");
        if path.is_file() {
            configure_obs_profile_recording_dir(&path, &videos_dir);
        }
    }
}

fn configure_obs_profile_recording_dir(path: &Path, videos_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };

    let desired = format!("RecFilePath={}", videos_dir.display());
    let mut changed = false;
    let mut in_recording_section = false;
    let mut saw_rec_file_path = false;
    let mut lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_recording_section && !saw_rec_file_path {
                lines.push(desired.clone());
                changed = true;
            }
            in_recording_section = matches!(trimmed, "[SimpleOutput]" | "[AdvOut]");
            saw_rec_file_path = false;
            lines.push(line.to_string());
            continue;
        }

        if trimmed.starts_with("RecFilePath=") {
            saw_rec_file_path = true;
            if trimmed == desired {
                lines.push(line.to_string());
            } else {
                lines.push(desired.clone());
                changed = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if in_recording_section && !saw_rec_file_path {
        lines.push(desired);
        changed = true;
    }

    if !changed {
        return;
    }

    let mut output = lines.join("\n");
    output.push('\n');
    if let Err(err) = std::fs::write(path, output) {
        flog(format!(
            "failed to update OBS recording directory in {}: {err}",
            path.display()
        ));
    }
}

impl BufferHandler for DesktopState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl OutputHandler for DesktopState {}

delegate_dispatch2!(DesktopState);
