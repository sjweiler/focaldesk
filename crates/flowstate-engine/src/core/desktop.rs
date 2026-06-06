use flowstate_types::types::{OutputId, WindowId, WorkspaceId};
use flowstate_ui::uitree::UiTree;
use smithay::backend::renderer::utils::import_surface_tree;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, PopupKind, PopupManager, Space, Window,
};
use smithay::wayland::compositor::get_parent;
use smithay::wayland::compositor::is_sync_subsurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::compositor::with_surface_tree_downward;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::core::output_store::OutputStore;
use crate::core::window_store::WindowStore;
use crate::core::workspace_store::WorkspaceStore;
use flowstate_ui::desktop_frame::DesktopFrameCtx;
use flowstate_ui::egui_layer::{EguiInputEvent, EguiModifiers, EguiPointerButton, EguiScrollDelta};
use flowstate_ui::types::{PanelKind, UiAction};
use smithay::backend::input::{Axis, AxisRelativeDirection, AxisSource, ButtonState};
use smithay::desktop::{WindowSurface, WindowSurfaceType};
use smithay::input::keyboard::keysyms;
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorIcon, MotionEvent};

use crate::core::shell::xwayland::{XwaylandSurfaceRole, XwaylandWindowMeta};
use crate::core::shell::WaylandWindowMeta;
use flowstate_cursor::CursorManager;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
#[cfg(feature = "xwayland")]
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::input::FlowKeyState;
use crate::core::input::FlowModifiers;
use crate::core::input::FlowMouseButton;
use crate::core::input::FlowScrollDelta;
use crate::core::input::FlowScrollSource;
use crate::core::input::{FlowInputEvent, InputState};
use crate::core::shell::ManagedWindow;
use crate::core::RenderState;
use flowstate_flow::actions::KeyAction;
use flowstate_flow::keybinds::BackendKind;
use flowstate_flow::Keybinds;
use flowstate_flow::ModMask;
use flowstate_logging::flog;
use flowstate_notifications::NotificationManager;
use flowstate_settings_core::AppSettings;
use flowstate_ui::chrome::Chrome;
use flowstate_ui::chrome::ChromeMetrics;
use indexmap::IndexMap;
use smithay::backend::input::AbsolutePositionEvent;
use smithay::delegate_output;
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
use tracing_subscriber::fmt::time;
use wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge};

use smithay::wayland::compositor;
use smithay::wayland::compositor::SurfaceAttributes;
use smithay::wayland::compositor::TraversalAction;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::wayland::xdg_activation::XdgActivationState;
#[cfg(feature = "xwayland")]
use smithay::wayland::xwayland_shell::XWaylandShellState;
#[cfg(feature = "xwayland")]
use smithay::xwayland::X11Wm;
use std::io::{self, Write};
use wayland_server::protocol::wl_surface;
use wayland_server::DisplayHandle;

use crate::core::chrome_layout::{
    build_chrome_layout, chrome_host_drag_hit, sidebar_slot_index_at,
};
use crate::core::focus::{KeyboardFocusTarget, PointerFocusTarget};
use crate::core::fonts::FontSystem;
use crate::core::toplevel_interaction::{
    cursor_for_resize_edges, handle_resize_surface_commit, resize_edges_at, ResizeEdgeMask,
    ResizeSurfaceState, ToplevelPointerInteraction, RESIZE_BORDER_PX,
};
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::FlowThemeId;
use flowstate_themes::ThemeManager;
use flowstate_ui::dialog::DialogAction;
use flowstate_ui::dialog::{Dialog, DialogId};
use flowstate_ui::dialog_layout::layout_dialog;

pub(crate) fn dbg_flush(msg: &str) {
    let mut stderr = io::stderr();
    let _ = writeln!(stderr, "{msg}");
    let _ = stderr.flush();
}

pub struct OutputState {
    pub handle: Output,
    pub physical_size: Size<i32, Physical>,
    pub logical_size: Size<i32, Logical>,
    pub logical_origin: Point<i32, Logical>,
    pub scale_factor: f64,
    pub scale: Scale<f64>,
    pub active_workspace: WorkspaceId,
    pub pending_damage: Vec<Rectangle<i32, Physical>>,
    pub last_sw_cursor_rect: Option<Rectangle<i32, Physical>>,
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
    pub backend_kind: BackendKind,
    pub cursor_manager: CursorManager,
    pub seat: Seat<DesktopState>,
    pub notifications: NotificationManager,
    pub chrome: flowstate_ui::chrome::Chrome,
    pub keybinds: Keybinds,
    pub client_wayland_display: String,
    pub theme_manager: ThemeManager,
    pub apps: AppSettings,
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
    pub next_window_id: WindowId,
    pub primary_output: OutputId,
    pub focused_output: OutputId, //keyboard shit
    pub input: InputState,
    // pub keybinds: Keybinds,
    pub running: bool,
    pub compositor_state: CompositorState,
    pub render: RenderState,
    pub xdg_shell_state: smithay::wayland::shell::xdg::XdgShellState,
    pub dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub dmabuf_node: Option<smithay::backend::drm::DrmNode>,
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
    pub portal_dispatch_ctx: Option<crate::core::portal::PortalDispatchCtx>,
    pub portal_frame_cache: HashMap<OutputId, crate::core::portal::PortalFrameCache>,
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
    pub chrome: flowstate_ui::chrome::Chrome,
    // focus/input
    pub seat_name: String,
    pub focused_window: Option<WindowId>,
    pub pointer_pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    /// In-progress interactive XDG move/resize driven by nested (winit) pointer events.
    pub toplevel_pointer: Option<ToplevelPointerInteraction>,

    // shell/chrome
    //pub topbar: TopBarModel,
    //pub sidebar: SidebarModel,
    pub notifications: NotificationManager,

    // xwayland and special surfaces
    pub unmapped_windows: Vec<ManagedWindow>,

    pub keybinds: Keybinds,

    pub client_wayland_display: String,
    pub apps: AppSettings,

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

    pub screenshot_requested: Option<OutputId>,
    pub screenshot_all_requested: bool,
    pub screenshot_seq: u64,

    pub fonts: FontSystem,

    pub theme: ThemeManager,
    //pub popups: Vec<PopupState>,
}

impl DesktopState {
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
        let old_hovered = self.ui.hovered;
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
            }
        }

        for rect in damage {
            self.mark_output_logical_damage(output_id, rect, 10);
        }

        true
    }

    /// Compositor chrome hit (sidebar/topbar UI), if any, without consuming the event.
    pub(crate) fn peek_ui_action_at_pointer(&self) -> Option<flowstate_ui::types::UiAction> {
        let local = self.pointer_relative_to_output_logical(self.focused_output)?;
        let x = local.x.round() as i32;
        let y = local.y.round() as i32;
        self.ui.hit_test(x, y).and_then(|el| el.action.clone())
    }

    pub fn click_ui_at_pointer(&mut self) -> bool {
        let Some(action) = self.peek_ui_action_at_pointer() else {
            return false;
        };
        flog(&format!("ACTION={:?}", action));
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
        let output_id = self.focused_output;
        let Some(frame_ctx) = self.egui_frame_ctx_for_output(output_id, Instant::now()) else {
            return;
        };
        self.sync_egui(&frame_ctx);
        self.mark_redraw();
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
        // Smithay `KeyboardKeyEvent::key_code()` is evdev-style, while
        // `smithay::input::keyboard::keysyms` are XKB keysyms.
        match keycode {
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

    fn handle_egui_input(&mut self, event: &FlowInputEvent) -> bool {
        if !self.render.egui.has_open_panels() {
            return false;
        }

        let egui_event = match *event {
            FlowInputEvent::PointerMoved { position } => {
                let Some(output_id) = self.output_under_pointer(position) else {
                    return false;
                };
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
            } => EguiInputEvent::Key {
                key: Self::egui_key(keycode),
                pressed: matches!(state, FlowKeyState::Pressed),
                repeat,
                modifiers: Self::egui_modifiers(modifiers),
            },
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
        self.space.element_bbox(window)
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
            self.mark_output_damage(output_id, rect);
        }
    }

    fn mark_window_bbox_damage(&mut self, rect: Rectangle<i32, Logical>) {
        self.mark_global_logical_damage(rect);
    }

    fn mark_output_logical_damage(
        &mut self,
        output_id: OutputId,
        rect: Rectangle<i32, Logical>,
        margin: i32,
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
        self.mark_output_damage(output_id, physical);
    }

    fn software_cursor_damage_pending_for_output(&self, output_id: OutputId) -> bool {
        self.output_contains_pointer(output_id)
            && self.cursor_manager.software_cursor_needed()
            && !self.drm_try_pass_cursor_this_frame
    }

    pub(crate) fn map_window_bbox_location(
        &mut self,
        window: Window,
        bbox_loc: Point<i32, Logical>,
        activate: bool,
    ) {
        let space_loc = bbox_loc + window.geometry().loc;
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
            let Some(global) = self.space.element_bbox(window) else {
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
        let under = self.pointer_surface_under(pos).map(|(s, p)| (s, p));
        static POINTER_FORWARD_LOGS: AtomicUsize = AtomicUsize::new(0);
        let seq = POINTER_FORWARD_LOGS.fetch_add(1, Ordering::Relaxed);
        if seq < 120 {
            flog(&format!(
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
        pos: Point<f64, Logical>,
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
            "nautilus" | "@files" => flowstate_files_command(),
            "@settings" | "flowstate-settings" => flowstate_settings_command(),
            other => other.to_string(),
        }
    }

    pub(crate) fn dispatch_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::LaunchApp(cmd) => {
                self.launch_app(self.resolve_launch_command(cmd));
            }

            UiAction::OpenPanel(panel) => {
                self.render.egui.open_panel(panel);
                self.mark_redraw();
            }

            UiAction::Custom(id) => match id {
                1001 => self.launch_app(flowstate_settings_command()),
                1004 => self.launch_app(self.apps.browser.clone()),
                1005 => self.launch_app(self.apps.terminal.clone()),
                1006 => self.launch_app(flowstate_files_command()),
                _ => eprintln!("unhandled custom ui action: {id}"),
            },

            UiAction::SystemCommand(cmd) => {
                eprintln!("TODO system command: {:?}", cmd);
            }

            UiAction::ToggleSetting(setting) => {
                eprintln!("TODO toggle setting: {:?}", setting);
            }
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
        self.screenshot_requested = Some(self.focused_output);
        self.mark_redraw();
        dbg_flush("SCREENSHOT REQUEST SET");
    }
    pub fn request_screenshot_all(&mut self) {
        self.screenshot_all_requested = true;
        self.mark_redraw();
        dbg_flush("SCREENSHOT ALL REQUEST SET");
    }

    pub fn take_screenshot_request(&mut self) -> Option<OutputId> {
        self.screenshot_requested.take()
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

    pub fn focused_workspace(&self) -> WorkspaceId {
        self.outputs
            .get(&self.focused_output)
            .map(|o| o.active_workspace)
            .unwrap_or(self.active_workspace)
    }

    pub fn set_focused_workspace(&mut self, workspace: WorkspaceId) {
        if let Some(output) = self.outputs.get_mut(&self.focused_output) {
            output.active_workspace = workspace;
        }
        self.active_workspace = workspace;
        self.mark_redraw();
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
                active_workspace: WorkspaceId(1),
                pending_damage: vec![Rectangle::from_loc_and_size((0, 0), physical_size)],
                last_sw_cursor_rect: None,
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

    fn work_recess_for_output(&self, output_id: OutputId) -> Option<Rectangle<i32, Logical>> {
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
            if window.x11_surface().is_none() {
                return None;
            }
            let bbox = self.global_window_bbox(window)?;
            let edges = resize_edges_at(bbox, px, py, RESIZE_BORDER_PX)?;
            Some((w.id, edges))
        })
    }

    fn update_pointer_cursor(&mut self, position: Point<f64, Logical>) {
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

    pub(crate) fn focus_window_id(&mut self, window_id: WindowId) {
        let Some(idx) = self.windows.iter().position(|w| w.id == window_id) else {
            return;
        };

        self.focused_window = Some(window_id);
        let window = self.windows[idx].window.clone();
        self.space.raise_element(&window, true);

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
        self.mark_redraw();
    }

    pub fn new(init: DesktopInit) -> Self {
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
            next_window_id: WindowId(1),
            primary_output: init.primary_output,
            focused_output: init.primary_output,
            input: InputState::default(),
            compositor_state: init.compositor_state,
            render: init.render,
            xdg_shell_state: init.xdg_shell_state,
            dmabuf_state: init.dmabuf_state,
            dmabuf_global: None,
            dmabuf_node: None,
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
            portal_dispatch_ctx: None,
            portal_frame_cache: HashMap::new(),
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

            notifications: init.notifications,
            unmapped_windows: Vec::new(),
            keybinds: init.keybinds,
            client_wayland_display: init.client_wayland_display,
            apps: init.apps,
            host_window_drag_requested: false,
            pending_compositor_move: None,
            pending_xdg_move: None,
            last_titlebar_click: None,
            suppress_next_left_release: false,
            running: init.running,
            drm_cursor_render_id: Id::new(),
            drm_submit_hw_cursor: false,
            drm_try_pass_cursor_this_frame: false,
            screenshot_requested: None,
            screenshot_all_requested: false,
            screenshot_seq: 0,
            theme: init.theme_manager,
        }
    }

    pub fn alloc_window_id(&mut self) -> WindowId {
        let id = self.next_window_id.0;
        self.next_window_id = WindowId(id.checked_add(1).expect("window id counter overflowed"));
        WindowId(id)
    }

    pub fn add_xdg_toplevel(&mut self, surface: ToplevelSurface) {
        dbg_flush("entered add_xdg_toplevel");
        dbg_flush(&format!("self={:p}", self));
        dbg_flush(&format!(
            "before map space={}",
            self.space.elements().count()
        ));

        let id = self.alloc_window_id();
        let window = Window::new_wayland_window(surface.clone());
        let workspace = self.focused_workspace();
        let meta = WaylandWindowMeta::new(None, None);

        //self.space.map_element(window.clone(), (100, 100), false);
        //dbg_flush(&format!("after map space={}", self.space.elements().count()));

        let managed = ManagedWindow::new_wayland(id, window.clone(), meta, workspace);
        self.windows.push(managed);
        dbg_flush(&format!("after push windows={}", self.windows.len()));

        self.mark_redraw();
    }

    #[cfg(feature = "xwayland")]
    pub fn add_xwayland_window(
        &mut self,
        surface: smithay::xwayland::X11Surface,
        override_redirect: bool,
    ) -> WindowId {
        let id = self.alloc_window_id();
        let window = Window::new_x11_window(surface.clone());
        let workspace = self.focused_workspace();
        let meta = XwaylandWindowMeta::from_surface(&surface)
            .with_override_redirect(override_redirect)
            .with_role(XwaylandSurfaceRole::from_surface(&surface));
        let mut managed = ManagedWindow::new_xwayland(id, window, meta.clone(), workspace);
        managed.floating = meta.should_float();
        managed.mapped = false;
        self.windows.push(managed);
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

        self.sync_xwayland_window_meta(&surface);

        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(true);
        }

        let Some(idx) = self.windows.iter().position(|window| window.id == id) else {
            return;
        };

        if surface.wl_surface().is_none() {
            let window = self.windows[idx].window.clone();
            self.space.unmap_elem(&window);
            self.windows[idx].mapped = false;
            flog(&format!(
                "XWayland map deferred id={:?}: no associated wl_surface yet",
                id
            ));
            self.mark_redraw();
            return;
        }

        let window = self.windows[idx].window.clone();
        let requested_geometry = surface.geometry();
        let output_id = self
            .output_under_pointer(self.input.pointer_pos)
            .unwrap_or(self.primary_output);
        let fallback_output = self.outputs.get(&output_id);

        let bbox_location = if surface.is_override_redirect() {
            requested_geometry.loc
        } else {
            self.default_toplevel_map_location(output_id)
        };

        self.windows[idx].float_rect = Some(Rectangle::from_loc_and_size(
            bbox_location,
            requested_geometry.size,
        ));

        window.on_commit();
        self.map_window_bbox_location(window.clone(), bbox_location, true);
        self.windows[idx].mapped = true;

        if !surface.is_override_redirect() {
            let configure_rect = self
                .space
                .element_bbox(&window)
                .filter(|bbox| bbox.size.w > 0 && bbox.size.h > 0)
                .unwrap_or_else(|| {
                    Rectangle::from_loc_and_size(bbox_location, requested_geometry.size)
                });
            flog(&format!(
                "XWayland map configure id={:?} requested={:?} configure={:?}",
                id, requested_geometry, configure_rect
            ));
            let _ = surface.configure(Some(configure_rect));
            self.focus_window_id(id);
        }

        self.space.refresh();
        self.mark_redraw();
    }

    pub fn open_dialog(&mut self, dialog: Dialog) {
        self.active_dialog = Some(dialog.id);
        self.dialogs.push(dialog);

        println!(
            "after open_dialog: dialogs={}, active_dialog={:?}",
            self.dialogs.len(),
            self.active_dialog
        );

        self.mark_redraw();
    }

    pub fn close_dialog(&mut self, id: DialogId) {
        self.dialogs.retain(|d| d.id != id);

        if self.active_dialog == Some(id) {
            self.active_dialog = None;
        }

        self.mark_redraw();
    }

    pub fn handle_dialog_action(&mut self, id: DialogId, action: DialogAction) {
        match action {
            DialogAction::Confirm => {
                println!("Dialog {} confirmed", id);

                // Example: allow screenshot
                // self.allow_screenshot = true;
            }

            DialogAction::Cancel => {
                println!("Dialog {} canceled", id);
            }

            DialogAction::Custom(v) => {
                println!("Dialog {} custom action {}", id, v);
            }
        }

        self.close_dialog(id);
    }

    /// Import the committed surface tree into the active renderer (during Wayland dispatch).
    pub fn early_import_surface(&mut self, surface: &WlSurface) {
        let Some(ctx) = self.portal_dispatch_ctx.as_mut() else {
            return;
        };
        // SAFETY: only called synchronously from `dispatch_clients` while ctx is set.
        let renderer = unsafe { &mut *ctx.renderer.as_ptr() };
        if let Err(err) = import_surface_tree(renderer, surface) {
            flowstate_logging::flog(&format!("early surface import failed: {err:?}"));
        }
    }

    pub fn handle_commit(&mut self, surface: &WlSurface) {
        dbg_flush("handle_commit hit");

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
                    if let Some(current_loc) = self.space.element_location(&window) {
                        flog(&format!(
                            "XWayland buffer_delta {:?} for window at {:?}",
                            buffer_offset, current_loc
                        ));
                        self.map_window_bbox_location(
                            window.clone(),
                            current_loc - window.geometry().loc + buffer_offset,
                            false,
                        );
                        if let Some(old_bbox) = old_bbox {
                            self.mark_window_bbox_damage(old_bbox);
                        }
                        if let Some(new_bbox) = self.global_window_bbox(&window) {
                            self.mark_window_bbox_damage(new_bbox);
                        }
                        commit_damage_queued = true;
                    }
                }
            }

            if let Some(idx) = self
                .windows
                .iter()
                .position(|managed| managed.window == window)
            {
                dbg_flush(&format!(
                    "commit matched window idx={idx} (toplevel or subsurface)"
                ));
                dbg_flush(&format!(
                    "already in space={}",
                    self.space.elements().any(|e| e == &window)
                ));
                dbg_flush(&format!("managed.mapped={}", self.windows[idx].mapped));
                let in_space = self.space.elements().any(|e| e == &window);
                if in_space && !self.windows[idx].mapped && !self.windows[idx].minimized {
                    self.windows[idx].mapped = true;
                    let window_id = self.windows[idx].id;
                    self.focus_window_id(window_id);
                    dbg_flush("marked existing space window mapped from commit");
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
                    dbg_flush(&format!(
                        "commit matched window idx={idx} (toplevel or subsurface)"
                    ));
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
            dbg_flush("mapped window from commit");
            dbg_flush(&format!(
                "space count after map={}",
                self.space.elements().count()
            ));
            mapped_window = true;
        }

        let resize_damage = handle_resize_surface_commit(&mut self.space, surface);
        if let Some((old_bbox, new_bbox)) = resize_damage {
            self.mark_window_bbox_damage(old_bbox);
            self.mark_window_bbox_damage(new_bbox);
        }

        self.ensure_popup_initial_configure(surface);

        if mapped_window {
            self.render.redraw_all = true;
            self.mark_redraw();
        } else if resize_damage.is_none() {
            if !commit_damage_queued {
                if let Some(window) = committed_window.as_ref() {
                    if let Some(bbox) = self.global_window_bbox(window) {
                        self.mark_window_bbox_damage(bbox);
                        commit_damage_queued = true;
                    }
                }
            }

            if !commit_damage_queued {
                self.render.redraw_all = true;
                self.mark_redraw();
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
        let Ok(root) = find_popup_root_surface(&PopupKind::from(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_wl_surface(&root) else {
            return;
        };

        let mut outputs_for_window = self.space.outputs_for_element(&window);
        if outputs_for_window.is_empty() {
            return;
        }

        let mut outputs_geo = self
            .space
            .output_geometry(&outputs_for_window.pop().unwrap())
            .unwrap();
        for output in outputs_for_window {
            outputs_geo = outputs_geo.merge(self.space.output_geometry(&output).unwrap());
        }

        let window_geo = self.space.element_geometry(&window).unwrap();

        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::from(popup.clone()));
        target.loc -= window_geo.loc;

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
                println!("Quit");
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

            KeyAction::ToggleLauncher => {
                self.render.egui.open_panel(PanelKind::AppLauncher);
                self.mark_redraw();
            }

            KeyAction::ActivateSlot(n) => {
                println!("Activate slot {} (not implemented yet)", n);
            }

            KeyAction::AssignSlot(n) => {
                println!("Assign slot {} (not implemented yet)", n);
            }

            KeyAction::OverflowView => {
                println!("Overflow view (not implemented yet)");
            }

            KeyAction::TakeScreenshot => {
                self.request_screenshot();
                dbg_flush("SCREENSHOT ACTION FIRED");
            }

            KeyAction::TakeScreenshotAll => {
                self.request_screenshot_all();
                dbg_flush("SCREENSHOT ALL ACTION FIRED");
            }

            KeyAction::ForceExit => {
                panic!("Emergency shutdown");
            }

            KeyAction::LaunchBrowser => {
                todo!();
            }

            KeyAction::LaunchFiles => {
                self.launch_app(flowstate_files_command());
            }
        }
    }

    pub fn close_focused(&mut self) {
        let Some(focused_id) = self.focused_window else {
            return;
        };

        let Some(managed) = self.window(focused_id) else {
            self.focused_window = None;
            return;
        };

        managed.request_close();
        self.mark_redraw();
    }

    pub fn activate_slot(&mut self, slot: usize) {
        println!("Activate slot {} (not implemented yet)", slot);
    }

    pub fn assign_slot(&mut self, slot: usize) {
        println!("Assign slot {} (not implemented yet)", slot);
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

        #[cfg(feature = "xwayland")]
        let xwayland_display = self.xwayland_display.as_deref();

        #[cfg(not(feature = "xwayland"))]
        let xwayland_display: Option<&str> = None;

        let display_env = xwayland_display.map(str::to_string);
        let launch_candidates = if chrome_like {
            chrome_exec_fallbacks(&app_name)
        } else {
            vec![app_name.clone()]
        };

        let mut last_error = None;
        for candidate in launch_candidates {
            let mut command = Command::new(&candidate);
            command.env("WAYLAND_DISPLAY", &self.client_wayland_display);
            command.env_remove("DISPLAY");
            if let Some(display) = xwayland_display {
                command.env("DISPLAY", display);
            }
            if chrome_like {
                configure_chrome_command(&mut command);
            }

            match command.spawn() {
                Ok(child) => {
                    flog(&format!(
                        "launched {candidate} pid={} WAYLAND_DISPLAY={} DISPLAY={display_env:?}",
                        child.id(),
                        self.client_wayland_display,
                    ));
                    if xwayland_display.is_none() && !chrome_like {
                        flog(
                            "warning: launched without DISPLAY; X11 apps (Steam/Proton/Wine) need XWayland",
                        );
                    }
                    return;
                }
                Err(err) => {
                    last_error = Some((candidate, err));
                }
            }
        }

        if let Some((candidate, err)) = last_error {
            flog(&format!("failed to launch {candidate}: {err}"));
            eprintln!("failed to launch {candidate}: {err}");
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
            //eprintln!("no keyboard on seat");
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

                flog(&format!(
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
            flog(&format!("ACTION={:?}", action,));

            self.handle_action(action);
        }
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
                        self.mark_window_bbox_damage(old_bbox);
                    }
                    if let Some(new_bbox) = new_bbox {
                        self.mark_window_bbox_damage(new_bbox);
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
        self.mark_redraw();
    }

    pub fn handle_input(&mut self, event: FlowInputEvent) {
        match event {
            FlowInputEvent::Key { keycode, state, .. } => {
                if matches!(state, FlowKeyState::Pressed) && keycode == 1 {
                    if self.render.egui.has_open_panels() {
                        self.render.egui.close_all_panels();
                        self.mark_redraw();
                        return;
                    }
                }
                if self.handle_egui_input(&event) {
                    self.mark_redraw();
                    return;
                }
                // Modal dialogs intercept inside `keyboard.input` (still updates XKB / modifier state).
                self.handle_key_event(keycode, state);
                self.mark_redraw();
            }

            FlowInputEvent::PointerMoved { position } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;
                if let Some(id) = self.output_under_pointer(position) {
                    self.focused_output = id;
                }
                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                    if self.render.egui.wants_pointer_input() {
                        self.clear_client_pointer_focus(self.pointer_pos);
                        self.mark_redraw();
                        return;
                    }
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
                    return;
                }
                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
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
                if !precise_toplevel_damage && !precise_hover_damage && !precise_cursor_damage {
                    self.mark_redraw();
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
                    self.focused_output = id;
                }

                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
                    return;
                }

                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
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
                    self.mark_redraw();
                    return;
                }

                if matches!(button, FlowMouseButton::Left)
                    && self.peek_ui_action_at_pointer().is_some()
                {
                    self.clear_client_pointer_focus(position);
                    match state {
                        FlowKeyState::Pressed => {
                            self.input.pointer_left_down = true;
                            self.ui.pressed = self
                                .pointer_relative_to_output_logical(self.focused_output)
                                .and_then(|local| {
                                    self.ui
                                        .hit_test(local.x.round() as i32, local.y.round() as i32)
                                        .map(|el| el.id)
                                });
                            let _ = self.click_ui_at_pointer();
                        }
                        FlowKeyState::Released => {
                            self.input.pointer_left_down = false;
                            self.ui.pressed = None;
                            self.pending_compositor_move = None;
                        }
                    }
                    self.mark_redraw();
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
                    self.mark_redraw();
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
                        self.mark_redraw();
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
                    self.focused_output = id;
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
                        self.mark_redraw();
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
                            } else {
                                self.pending_compositor_move = self
                                    .top_mapped_window_id_at(position)
                                    .filter(|&id| {
                                        self.window(id).is_some_and(|w| {
                                            w.mapped
                                                && !w.maximized
                                                && !w.fullscreen
                                                && !w.minimized
                                        })
                                    })
                                    .map(|id| (id, position));
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
                if !precise_toplevel_damage && !precise_hover_damage && !precise_cursor_damage {
                    self.mark_redraw();
                }
            }

            FlowInputEvent::PointerScroll {
                position, delta, ..
            } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;
                if let Some(id) = self.output_under_pointer(position) {
                    self.focused_output = id;
                }
                if self.render.egui.has_open_panels() {
                    let _ = self.handle_egui_input(&event);
                    if self.render.egui.wants_pointer_input() {
                        self.clear_client_pointer_focus(self.pointer_pos);
                        self.mark_redraw();
                        return;
                    }
                } else if self.handle_egui_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
                    return;
                }
                if self.handle_dialog_input(&event) {
                    self.clear_client_pointer_focus(self.pointer_pos);
                    self.mark_redraw();
                    return;
                }
                self.cursor_manager.move_to(position.x, position.y);
                if !self.compositor_pointer_grab_active() {
                    self.forward_pointer_to_clients(position);
                    self.forward_pointer_scroll(delta);
                }
                self.mark_redraw();
            }

            FlowInputEvent::PointerEntered => {
                self.cursor_manager.set_visible(true);
                self.mark_redraw();
            }

            FlowInputEvent::PointerLeft => {
                let _ = self.handle_egui_input(&event);
                self.process_toplevel_pointer_button(FlowMouseButton::Left, FlowKeyState::Released);
                self.input.pointer_left_down = false;
                self.pending_compositor_move = None;
                self.pending_xdg_move = None;
                self.cursor_manager.set_visible(false);
                self.mark_redraw();
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
                self.mark_redraw();
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

        self.mark_redraw();
    }

    pub fn needs_redraw(&self) -> bool {
        self.render.redraw_all
            || self
                .outputs
                .values()
                .any(|output| !output.pending_damage.is_empty())
    }

    pub fn clear_repaint_request(&mut self) {
        self.render.redraw_all = false;
        for output in self.outputs.values_mut() {
            output.pending_damage.clear();
        }
    }

    pub fn mark_redraw(&mut self) {
        self.render.redraw_all = true;
    }

    pub fn mark_output_damage(&mut self, output_id: OutputId, rect: Rectangle<i32, Physical>) {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }

        if let Some(output) = self.outputs.get_mut(&output_id) {
            let bounds = Rectangle::from_loc_and_size((0, 0), output.physical_size);
            if let Some(clipped) = rect.intersection(bounds) {
                output.pending_damage.push(clipped);
            }
        }
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

            // For now: just mark redraw
            // Later: update layout/output metrics properly
            self.render.redraw_all = true;
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
            Some(mode.clone()),
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
                    active_workspace: WorkspaceId(1),
                    pending_damage: vec![Rectangle::from_loc_and_size((0, 0), size)],
                    last_sw_cursor_rect: None,
                },
            );
        }

        self.cursor_manager
            .set_base_size_and_scale(24, scale as f32);
    }

    pub fn insert_nested_output(&mut self, output: Output, size: Size<i32, Physical>, scale: f64) {}

    pub fn tick_layout(&mut self) {
        self.popups.cleanup();
    }

    /// Update output enter/leave and refresh mapped client surfaces. Call before flushing Wayland clients.
    pub fn refresh_space(&mut self) {
        self.space.refresh();
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
                flowstate_logging::flog(&format!(
                    "frame surface import failed: {err:?}; root_buffer={buffer_info}"
                ));
            }
        }
    }

    pub fn send_frame_callbacks(&mut self, _millis: u32) {
        for surface in self.xdg_shell_state.toplevel_surfaces().iter() {
            Self::send_frames_surface_tree(surface.wl_surface(), _millis);
        }

        #[cfg(feature = "xwayland")]
        {
            let time = Duration::from_millis(_millis.into());
            let fallback_output = self
                .outputs
                .get(&self.focused_output)
                .or_else(|| self.outputs.get(&self.primary_output))
                .map(|output| output.handle.clone());

            for window in self.space.elements() {
                if window.x11_surface().is_none() {
                    continue;
                }

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
    }

    fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
        with_surface_tree_downward(
            surface,
            (),
            |_, _, &()| TraversalAction::DoChildren(()),
            |_surface, states, &()| {
                for callback in states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .frame_callbacks
                    .drain(..)
                {
                    callback.done(time);
                }
            },
            |_, _, &()| true,
        );
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

        if maximized {
            let Some(work) = self.work_recess_for_output(output_id) else {
                self.windows[idx].set_maximized(true);
                self.mark_redraw();
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

            self.map_window_bbox_location(window, work.loc, true);
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

            self.map_window_bbox_location(window, restore_rect.loc, true);
        }

        self.space.refresh();
        self.mark_redraw();
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
                self.mark_redraw();
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

            self.map_window_bbox_location(window, rect.loc, true);
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

            self.map_window_bbox_location(window, restore_rect.loc, true);
        }

        self.space.refresh();
        self.mark_redraw();
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
                self.mark_output_damage(output_id, Self::expand_physical_rect(old_rect, 4));
            }
            return Ok(());
        }

        self.render
            .upload_cursor_texture_for_desktop(renderer, &mut self.cursor_manager)?;

        let need_sw =
            self.cursor_manager.software_cursor_needed() && !self.drm_try_pass_cursor_this_frame;
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
                    self.mark_output_damage(output_id, Self::expand_physical_rect(old_rect, 4));
                }
                self.mark_output_damage(output_id, Self::expand_physical_rect(cursor_rect, 4));
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
                self.mark_output_damage(output_id, Self::expand_physical_rect(old_rect, 4));
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
        .join("flowstate")
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
                flog(&format!(
                    "failed to remove stale Chrome singleton {}: {err}",
                    path.display()
                ));
            }
        }
    }
    flog(&format!(
        "removed stale Chrome profile singleton for pid {pid}"
    ));
}

fn flowstate_files_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("flowstate-files")))
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "flowstate-files".to_string())
}

fn flowstate_settings_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("flowstate-settings"))
        })
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "flowstate-settings".to_string())
}

fn configure_chrome_command(command: &mut Command) {
    let profile = chrome_profile_dir();
    clear_stale_chrome_singleton(&profile);
    command
        .arg("--ozone-platform=wayland")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--new-window");
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
            | "chromium"
            | "chromium-browser"
    )
}

impl BufferHandler for DesktopState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl OutputHandler for DesktopState {}

delegate_output!(DesktopState);
