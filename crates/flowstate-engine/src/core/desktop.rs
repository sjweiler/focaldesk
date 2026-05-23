use flowstate_types::types::{OutputId, WindowId, WorkspaceId};
use flowstate_ui::uitree::UiTree;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, PopupKind, PopupManager, Space, Window,
};
use smithay::wayland::compositor::with_surface_tree_downward;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::core::output_store::OutputStore;
use crate::core::window_store::WindowStore;
use crate::core::workspace_store::WorkspaceStore;
use flowstate_ui::egui_layer::{EguiInputEvent, EguiModifiers, EguiPointerButton, EguiScrollDelta};
use flowstate_ui::types::UiAction;
use smithay::backend::input::{Axis, AxisSource, ButtonState};
use smithay::desktop::WindowSurfaceType;
use smithay::input::keyboard::keysyms;
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};

use crate::core::shell::xwayland::{XwaylandSurfaceRole, XwaylandWindowMeta};
use crate::core::shell::WaylandWindowMeta;
use flowstate_cursor::CursorManager;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use std::borrow::Cow;

use crate::core::input::FlowKeyState;
use crate::core::input::FlowModifiers;
use crate::core::input::FlowMouseButton;
use crate::core::input::FlowScrollDelta;
use crate::core::input::{FlowInputEvent, InputState};
use crate::core::shell::ManagedWindow;
use crate::core::RenderState;
use flowstate_flow::actions::KeyAction;
use flowstate_flow::keybinds::BackendKind;
use flowstate_flow::Keybinds;
use flowstate_flow::ModMask;
use flowstate_notifications::NotificationManager;
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
use std::process::id;
use std::process::Command;
use tracing_subscriber::fmt::time;
use wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge};

use smithay::wayland::compositor::TraversalAction;
use smithay::wayland::compositor::{self, SurfaceAttributes};
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::wayland::xdg_activation::XdgActivationState;
use std::io::{self, Write};
use wayland_server::protocol::wl_surface;

use crate::core::chrome_layout::{
    build_chrome_layout, chrome_host_drag_hit, sidebar_slot_index_at,
};
use crate::core::focus::KeyboardFocusTarget;
use crate::core::fonts::FontSystem;
use crate::core::toplevel_interaction::{
    handle_resize_surface_commit, ResizeEdgeMask, ResizeSurfaceState, ToplevelPointerInteraction,
};
use flowstate_logging::flog;
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::FlowThemeId;
use flowstate_themes::ThemeManager;
use flowstate_ui::dialog::DialogAction;
use flowstate_ui::dialog::DialogButton;
use flowstate_ui::dialog::DialogKind;
use flowstate_ui::dialog::DialogState;
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
}

pub struct DesktopInit {
    pub xdg_activation_state: XdgActivationState,
    pub primary_output: OutputId,
    pub running: bool,
    pub compositor_state: CompositorState,
    pub render: RenderState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: smithay::input::SeatState<DesktopState>,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
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
}

pub struct DesktopState {
    // smithay protocol state
    pub xdg_activation_state: XdgActivationState,
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
    pub shm_state: smithay::wayland::shm::ShmState,
    pub seat_state: smithay::input::SeatState<Self>,
    pub output_manager_state: smithay::wayland::output::OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub layer_shell_state: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_source_state:
        smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub image_copy_capture_state: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    pub portal_dispatch_ctx: Option<crate::core::portal::PortalDispatchCtx>,
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

    /// Undecorated winit window: set on left-press over chrome top bar; backend calls platform window drag.
    host_window_drag_requested: bool,

    /// Left press on a client in the work area: after pointer moves past a threshold, start compositor move.
    /// (Nested mode does not forward pointer to clients, so xdg_toplevel.move from apps never runs.)
    pending_compositor_move: Option<(WindowId, Point<f64, Logical>)>,

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
    pub fn update_ui_hover_for_output(&mut self, output_id: OutputId) {
        if !self.output_contains_pointer(output_id) {
            self.ui.hovered = None;

            for el in &mut self.ui.elements {
                el.hovered = false;
            }

            return;
        }

        let Some(rel) = self.pointer_relative_to_output_logical(output_id) else {
            return;
        };
        let x = rel.x.round() as i32;
        let y = rel.y.round() as i32;

        self.ui.hovered = self.ui.hit_test(x, y).map(|e| e.id);

        for el in &mut self.ui.elements {
            el.hovered = Some(el.id) == self.ui.hovered;
        }
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

    /// Topmost client subsurface or xdg popup under `pos` (global logical), if any.
    pub(crate) fn pointer_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let ws = self.focused_workspace();
        for w in self.space.elements().rev() {
            let Some(managed) = self.windows.iter().find(|mw| mw.mapped && &mw.window == w) else {
                continue;
            };
            if managed.workspace != ws {
                continue;
            }
            let Some(loc) = self.space.element_location(w) else {
                continue;
            };
            let bbox = w.bbox_with_popups();
            let global = Rectangle::from_loc_and_size(bbox.loc + loc, bbox.size);
            if !global.contains(pos.to_i32_round()) {
                continue;
            }
            if let Some((surf, surf_loc)) =
                w.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
            {
                let global_surf_origin = surf_loc.to_f64() + loc.to_f64();
                return Some((surf, global_surf_origin));
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

    /// Deliver pointer motion to Wayland clients (nested compositor path).
    pub fn forward_pointer_to_clients(&mut self, pos: Point<f64, Logical>) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let under = self.pointer_surface_under(pos).map(|(s, p)| (s, p));
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
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    pub(crate) fn dispatch_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::LaunchApp(cmd) => {
                self.launch_app(cmd.to_string());
                //let _ = std::process::Command::new(cmd.to_string*().spawn();
            }

            UiAction::OpenPanel(panel) => {
                self.render.egui.open_panel(panel);
                self.mark_redraw();
            }

            UiAction::Custom(id) => {
                eprintln!("TODO custom ui action: {}", id);

                // Example: workspace slot IDs
                // self.set_focused_workspace(WorkspaceId(id as u64));
            }

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
            }
        });

        entry.handle = handle;
        entry.physical_size = physical_size;
        entry.logical_size = logical_size;
        entry.logical_origin = logical_origin;
        entry.scale_factor = scale_factor;
        entry.scale = Scale::from((scale_factor, scale_factor));

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
        self.windows
            .iter()
            .rev()
            .find(|managed| managed.mapped && managed.bbox().contains((px, py)))
            .map(|managed| managed.id)
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

        if self.windows[idx].wl_surface().is_some() {
            if let Some(keyboard) = self.seat.get_keyboard() {
                let serial = SERIAL_COUNTER.next_serial();
                keyboard.set_focus(self, Some(KeyboardFocusTarget::Window(window)), serial);
            }
        }
    }

    fn focus_window_at(&mut self, position: Point<f64, Logical>) {
        let px = position.x.round() as i32;
        let py = position.y.round() as i32;

        // Iterate from top-most to bottom-most.
        let target_id = self
            .windows
            .iter()
            .rev()
            .find(|managed| {
                if !managed.mapped {
                    return false;
                }
                managed.bbox().contains((px, py))
            })
            .map(|managed| managed.id);

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
            xdg_activation_state: init.xdg_activation_state,
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
            shm_state: init.shm_state,
            seat_state: init.seat_state,
            output_manager_state: init.output_manager_state,
            data_device_state: init.data_device_state,
            layer_shell_state: init.layer_shell_state,
            image_capture_source_state: init.image_capture_source_state,
            output_capture_source_state: init.output_capture_source_state,
            image_copy_capture_state: init.image_copy_capture_state,
            portal_dispatch_ctx: None,
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
            host_window_drag_requested: false,
            pending_compositor_move: None,
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

    pub fn handle_commit(&mut self, surface: &WlSurface) {
        dbg_flush("handle_commit hit");

        self.popups.commit(surface);

        let mut to_map: Option<usize> = None;

        for (idx, managed) in self.windows.iter().enumerate() {
            let mut belongs = false;
            managed.window.with_surfaces(|s, _| {
                if s == surface {
                    belongs = true;
                }
            });

            if belongs {
                managed.window.on_commit();
                dbg_flush(&format!(
                    "commit matched window idx={idx} (toplevel or subsurface)"
                ));
                dbg_flush(&format!(
                    "already in space={}",
                    self.space.elements().any(|e| e == &managed.window)
                ));
                dbg_flush(&format!("managed.mapped={}", managed.mapped));

                if !self.space.elements().any(|e| e == &managed.window) {
                    to_map = Some(idx);
                }
                break;
            }
        }

        if let Some(idx) = to_map {
            let window = self.windows[idx].window.clone();

            let output_id = self
                .output_under_pointer(self.input.pointer_pos)
                .unwrap_or(self.primary_output);

            let (mx, my) = if let Some(out) = self.outputs.get(&output_id) {
                (out.logical_origin.x + 100, out.logical_origin.y + 100)
            } else {
                (100, 100)
            };

            self.space.map_element(window, (mx, my), false);
            self.windows[idx].mapped = true;
            let window_id = self.windows[idx].id;
            self.focus_window_id(window_id);
            dbg_flush("mapped window from commit");
            dbg_flush(&format!(
                "space count after map={}",
                self.space.elements().count()
            ));
        }

        let _ = handle_resize_surface_commit(&mut self.space, surface);

        self.ensure_popup_initial_configure(surface);

        self.render.redraw_all = true;
        self.mark_redraw();
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
                println!("Launch terminal");

                let _ = Command::new("weston-terminal")
                    .env("WAYLAND_DISPLAY", &self.client_wayland_display)
                    .env_remove("DISPLAY")
                    .spawn();
            }

            KeyAction::ToggleLauncher => {
                let owner_output = self.focused_output;
                //let output = self.outputs.get(&output_id).unwrap();

                let dialog_w = 800;
                let dialog_h = 480;

                let (x, y) = if let Some((_id, output)) = self.outputs.iter().next() {
                    let output_width = output.logical_size.w;
                    let output_height = output.logical_size.h;

                    let dialog_w = 800;
                    let dialog_h = 480;

                    (
                        (output_width - dialog_w) / 2,
                        (output_height - dialog_h) / 2,
                    )
                } else {
                    (560, 300)
                };

                let dialog = Dialog {
                    id: 1,
                    kind: DialogKind::Info,
                    title: "FlowState Test".into(),
                    message: "Dialog system is alive.".into(),
                    owner_output,
                    buttons: vec![
                        DialogButton {
                            label: "OK".into(),
                            action: DialogAction::Confirm,
                        },
                        DialogButton {
                            label: "Cancel".into(),
                            action: DialogAction::Cancel,
                        },
                    ],
                    modal: true,
                    dismissible: true,
                    state: DialogState::Open,
                    bounds: Rectangle::<i32, Logical>::from_loc_and_size(
                        (x, y),
                        (dialog_w, dialog_h),
                    ),
                };

                self.open_dialog(dialog);
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
                todo!();
            }
        }
    }

    pub fn close_focused(&mut self) {
        let Some(focused_id) = self.focused_window else {
            return;
        };

        let Some(idx) = self.windows.iter().position(|w| w.id == focused_id) else {
            self.focused_window = None;
            return;
        };

        let managed = self.windows.remove(idx);
        self.space.unmap_elem(&managed.window);

        let next_focus = self.windows.iter().rev().find(|w| w.mapped).map(|w| w.id);

        self.focused_window = None;
        if let Some(next_id) = next_focus {
            self.focus_window_id(next_id);
        } else if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Option::<KeyboardFocusTarget>::None, serial);
        }

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
        use std::process::Command;

        let _ = Command::new("alacritty").spawn();
    }

    pub fn launch_app(&self, app: String) {
        let app_name = app.clone();
        if let Err(err) = Command::new(app)
            .env("WAYLAND_DISPLAY", &self.client_wayland_display)
            .env_remove("DISPLAY")
            .spawn()
        {
            eprintln!("failed to launch {app_name}: {err}");
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

    fn process_toplevel_pointer_motion(&mut self, pos: Point<f64, Logical>) {
        match &self.toplevel_pointer {
            Some(ToplevelPointerInteraction::Move {
                window_id,
                pointer_start,
                initial_location,
            }) => {
                let delta = pos - *pointer_start;
                let new_loc = (initial_location.to_f64() + delta).to_i32_round();
                if let Some(w) = self.window(*window_id) {
                    self.space.map_element(w.window.clone(), new_loc, true);
                }
            }
            Some(ToplevelPointerInteraction::Resize {
                window_id,
                edges,
                pointer_start,
                initial_rect,
                ..
            }) => {
                let mut delta = pos - *pointer_start;

                let mut new_window_width = initial_rect.size.w;
                let mut new_window_height = initial_rect.size.h;

                let e = *edges;
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

                let Some(w) = self.window(*window_id) else {
                    return;
                };
                let Some(tl) = w.window.toplevel() else {
                    return;
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
            }
            None => {}
        }
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
            ToplevelPointerInteraction::Move { .. } => {}
        }
        self.mark_redraw();
    }

    pub fn handle_input(&mut self, event: FlowInputEvent) {
        match event {
            FlowInputEvent::Key { keycode, state, .. } => {
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
                if self.handle_egui_input(&event) {
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
                // Nested compositor: start moving a client after a small drag from a work-area press.
                const DRAG_THRESHOLD_SQ: f64 = 5.0 * 5.0;
                if self.input.pointer_left_down {
                    if let Some((id, start)) = self.pending_compositor_move {
                        let d = position - start;
                        if d.x * d.x + d.y * d.y >= DRAG_THRESHOLD_SQ {
                            self.pending_compositor_move = None;
                            self.try_begin_compositor_move(id);
                        }
                    }
                }
                self.process_toplevel_pointer_motion(position);
                self.forward_pointer_to_clients(position);
                self.mark_redraw();
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

                if self.handle_egui_input(&event) {
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
                    && matches!(state, FlowKeyState::Pressed)
                    && self.peek_ui_action_at_pointer().is_some()
                {
                    self.input.pointer_left_down = true;
                    self.clear_client_pointer_focus(position);
                    let _ = self.click_ui_at_pointer();
                    self.mark_redraw();
                    return;
                }

                if matches!(button, FlowMouseButton::Left) {
                    match state {
                        FlowKeyState::Pressed => {
                            self.input.pointer_left_down = true;
                        }
                        FlowKeyState::Released => {
                            self.input.pointer_left_down = false;
                            self.pending_compositor_move = None;
                        }
                    }
                }

                if let Some(id) = self.output_under_pointer(position) {
                    self.focused_output = id;
                }
                self.cursor_manager.move_to(position.x, position.y);
                self.process_toplevel_pointer_motion(position);
                self.forward_pointer_to_clients(position);
                self.forward_pointer_button(position, button, state);
                if matches!(state, FlowKeyState::Pressed) {
                    if matches!(button, FlowMouseButton::Left)
                        && self.pointer_on_chrome_host_drag_region(position)
                    {
                        self.host_window_drag_requested = true;
                        self.pending_compositor_move = None;
                    } else {
                        self.focus_window_at(position);
                        if matches!(button, FlowMouseButton::Left)
                            && self.pointer_in_work_recess(position)
                        {
                            self.pending_compositor_move = self
                                .top_mapped_window_id_at(position)
                                .filter(|&id| {
                                    self.window(id).is_some_and(|w| {
                                        w.mapped && !w.maximized && !w.fullscreen && !w.minimized
                                    })
                                })
                                .map(|id| (id, position));
                        }
                    }
                }
                self.process_toplevel_pointer_button(button, state);
                self.mark_redraw();
            }

            FlowInputEvent::PointerScroll {
                position, delta, ..
            } => {
                self.input.pointer_pos = position;
                self.pointer_pos = position;
                if let Some(id) = self.output_under_pointer(position) {
                    self.focused_output = id;
                }
                if self.handle_egui_input(&event) {
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
                self.forward_pointer_to_clients(position);
                self.forward_pointer_scroll(delta);
                self.mark_redraw();
            }

            FlowInputEvent::PointerEntered => {
                self.cursor_manager.set_visible(true);
                self.mark_redraw();
            }

            FlowInputEvent::PointerLeft => {
                let _ = self.handle_egui_input(&event);
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

    pub fn update_output_size(
        &mut self,
        output_id: OutputId,
        physical_size: Size<i32, Physical>,
        scale_factor: f64,
    ) {
        if let Some(output) = self.outputs.get_mut(&output_id) {
            output.scale_factor = scale_factor;
            output.scale = Scale::from((scale_factor, scale_factor));
            output.physical_size = physical_size;

            let logical_w = (physical_size.w as f64 / scale_factor).round() as i32;
            let logical_h = (physical_size.h as f64 / scale_factor).round() as i32;

            output.logical_size = Size::<i32, Logical>::from((logical_w, logical_h));
        }

        self.mark_redraw();
    }

    pub fn needs_redraw(&self) -> bool {
        self.render.redraw_all
    }

    pub fn clear_repaint_request(&mut self) {
        self.render.redraw_all = false;
    }

    pub fn mark_redraw(&mut self) {
        self.render.redraw_all = true;
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
            Some(OutputScaleSmithay::Integer(scale_int)),
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

    pub fn send_frame_callbacks(&mut self, _millis: u32) {
        for surface in self.xdg_shell_state.toplevel_surfaces().iter() {
            Self::send_frames_surface_tree(surface.wl_surface(), _millis);
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

    pub fn window_id_for_toplevel(&self, surface: &ToplevelSurface) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|w| w.matches_toplevel(surface))
            .map(|w| w.id)
    }

    pub fn lookup_window_id_for_surface(&self, surface: &ToplevelSurface) -> Option<WindowId> {
        self.window_id_for_toplevel(surface)
    }

    pub fn request_move(&mut self, id: WindowId) {
        let Some(w) = self.window(id) else {
            return;
        };
        let Some(loc) = self.space.element_location(&w.window) else {
            return;
        };
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
        let edges_m = ResizeEdgeMask::from(edges);
        let Some(w) = self.window(id) else {
            return;
        };
        let Some(initial_rect) = self.space.element_bbox(&w.window) else {
            return;
        };
        let Some(tl) = w.window.toplevel() else {
            return;
        };
        ResizeSurfaceState::set_resizing(tl.wl_surface(), edges_m, initial_rect);
        let last_window_size = initial_rect.size;
        self.toplevel_pointer = Some(ToplevelPointerInteraction::Resize {
            window_id: id,
            edges: edges_m,
            pointer_start: self.pointer_pos,
            initial_rect,
            last_window_size,
        });
        if let Some(window) = self.window_mut(id) {
            window.pending_resize = None;
        }
    }

    pub fn request_maximize(&mut self, id: WindowId) {
        if let Some(window) = self.window_mut(id) {
            window.set_maximized(true);
        }
    }

    pub fn request_fullscreen(&mut self, id: WindowId) {
        if let Some(window) = self.window_mut(id) {
            window.set_fullscreen(true);
        }
    }

    pub fn prepare_cursor_for_frame(
        &mut self,
        renderer: &mut GlesRenderer,
        output_id: OutputId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(desk_output) = self.outputs.get(&output_id) else {
            return Ok(());
        };
        self.cursor_manager
            .set_base_size_and_scale(24, desk_output.scale_factor as f32);
        self.cursor_manager
            .move_to(self.pointer_pos.x, self.pointer_pos.y);

        if !self.cursor_manager.visible() {
            self.render.clear_sw_cursor_texture();
            self.render.sw_cursor_dst_rect = None;
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
                rel.to_physical_precise_round::<f64, i32>(desk_output.scale);
            let (hx, hy) = self.render.sw_cursor_hotspot;
            let (tw, th) = self.render.sw_cursor_tex_size;
            self.render.sw_cursor_dst_rect = Some((phys.x - hx, phys.y - hy, tw, th));
        } else {
            self.render.sw_cursor_dst_rect = None;
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

impl BufferHandler for DesktopState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl OutputHandler for DesktopState {}

delegate_output!(DesktopState);
