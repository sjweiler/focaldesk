//! VUI-rendered content hosted by the trusted GTK layer-shell clients.
//!
//! GTK owns the Wayland layer surface and input lifecycle; VUI owns the widget
//! tree, layout, interaction state, and Vello rendering. Readback is currently
//! used as the GTK bridge until GTK exposes a shareable wgpu render target.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

use focaldesk_ipc::{
    send_desktop_request, DesktopAction, DesktopSnapshot, IpcRequest, IpcResponse, ShellPanel,
    WindowSnapshot,
};
use gtk::{cairo, glib, prelude::*};
use gtk4_layer_shell::LayerShell;
use vui_core::{
    BoxConstraints, Button, ButtonVariant, Column, CrossAxisAlignment, Event, EventCtx, Flex,
    FlexChild, IconButton, IconData, IconName, Insets, Label, LayoutCtx, PaintCtx, PathCommand,
    Point, PointerButton, Rect, SemanticsCtx, Size, Spacer, Stack, Surface, Theme, UiRuntime,
    Widget,
};
use vui_text::ParleyTextSystem;
use vui_vello::ReadbackRenderer;

use crate::ShellRole;

struct ClientState {
    runtime: UiRuntime,
    renderer: ReadbackRenderer,
    rendered: Option<cairo::ImageSurface>,
    started: Instant,
    scale: f64,
    snapshot_signature: Vec<String>,
    tooltip_targets: Vec<(vui_core::Rect, String)>,
    first_frame_logged: bool,
}

const DOCK_HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

#[derive(Clone, Default)]
struct DockViewState {
    chooser: Option<Vec<WindowSnapshot>>,
    page: usize,
}

#[derive(Default)]
struct DockVisibilityState {
    pointer_inside: bool,
    hidden: bool,
    hide_after: Option<Instant>,
}

pub(super) fn enabled(role: ShellRole) -> bool {
    fn flag(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    }

    flag("FOCALDESK_VUI")
        && match role {
            ShellRole::Panel => flag("FOCALDESK_VUI_RAIL"),
            ShellRole::Dock => flag("FOCALDESK_VUI_DOCK"),
        }
}

pub(super) fn build(
    window: &gtk::ApplicationWindow,
    role: ShellRole,
    connector: String,
) -> Option<gtk::DrawingArea> {
    let renderer = match ReadbackRenderer::new() {
        Ok(renderer) => renderer,
        Err(error) => {
            focaldesk_logging::flog_warn!("cannot initialize VUI layer client renderer: {error}");
            return None;
        }
    };
    let actions = Rc::new(RefCell::new(Vec::<DesktopAction>::new()));
    let latest_snapshot = Rc::new(RefCell::new(None::<DesktopSnapshot>));
    let dock_view = Rc::new(RefCell::new(DockViewState::default()));
    let dock_visibility = Rc::new(RefCell::new(DockVisibilityState::default()));
    let pending_dock_hover = Rc::new(RefCell::new(None::<(Point, Instant)>));
    let runtime = build_runtime(
        role,
        &connector,
        actions.clone(),
        latest_snapshot.clone(),
        dock_view.clone(),
    );

    let state = Rc::new(RefCell::new(ClientState {
        runtime,
        renderer,
        rendered: None,
        started: Instant::now(),
        scale: 1.0,
        snapshot_signature: Vec::new(),
        tooltip_targets: Vec::new(),
        first_frame_logged: false,
    }));
    let first_frame_ready = Rc::new(Cell::new(false));
    let ready_reported = Rc::new(Cell::new(false));
    let ready_frame_state = first_frame_ready.clone();
    let ready_reported_state = ready_reported.clone();
    let ready_namespace = role.namespace().to_owned();
    let ready_connector = connector.clone();
    window.connect_map(move |window| {
        let Some(frame_clock) = window.frame_clock() else {
            return;
        };
        let frame_ready = ready_frame_state.clone();
        let reported = ready_reported_state.clone();
        let namespace = ready_namespace.clone();
        let connector = ready_connector.clone();
        frame_clock.connect_after_paint(move |_| {
            if !frame_ready.get() || reported.replace(true) {
                return;
            }
            let namespace = namespace.clone();
            let connector = connector.clone();
            std::thread::spawn(move || {
                match send_desktop_request(&IpcRequest::ShellReady {
                    namespace: namespace.clone(),
                    output_count: 1,
                    connector: Some(connector.clone()),
                    ready: true,
                }) {
                    Ok(IpcResponse::Ok) => focaldesk_logging::flog_warn!(
                        "VUI shell first frame ready: namespace={namespace} connector={connector}"
                    ),
                    Ok(response) => focaldesk_logging::flog_warn!(
                        "VUI shell first-frame readiness got unexpected response: namespace={namespace} connector={connector} response={response:?}"
                    ),
                    Err(error) => focaldesk_logging::flog_warn!(
                        "VUI shell client could not report its first painted frame: namespace={namespace} connector={connector} error={error}"
                    ),
                }
            });
        });
    });
    let area = gtk::DrawingArea::new();
    area.set_content_width(match role {
        ShellRole::Panel => 64,
        ShellRole::Dock => 560,
    });
    area.set_content_height(match role {
        ShellRole::Panel => 1,
        ShellRole::Dock => 64,
    });
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_focusable(true);
    let map_role = role;
    let map_connector = connector.clone();
    area.connect_map(move |area| {
        focaldesk_logging::flog_warn!(
            "VUI GTK shell area mapped: role={map_role:?} connector={map_connector} size={}x{}",
            area.width(),
            area.height()
        );
    });

    let draw_state = state.clone();
    let draw_connector = connector.clone();
    area.set_draw_func(move |area, context, width, height| {
        let scale = f64::from(area.scale_factor().max(1));
        let mut state = draw_state.borrow_mut();
        if (state.scale - scale).abs() > f64::EPSILON {
            state.scale = scale;
        }
        state
            .runtime
            .set_viewport(Size::new(f64::from(width), f64::from(height)));
        if let Some(frame) = state.runtime.render_frame() {
            state.tooltip_targets = frame
                .semantics
                .iter()
                .filter(|node| {
                    node.role == vui_core::SemanticRole::Button && !node.label.is_empty()
                })
                .map(|node| (node.bounds, node.label.clone()))
                .collect();
            match state.renderer.render(&frame, scale) {
                Ok(mut image) => {
                    for pixel in image.pixels.chunks_exact_mut(4) {
                        // Cairo ARgb32 is native-endian premultiplied BGRA on
                        // the Linux targets supported by FocalDesk.
                        pixel.swap(0, 2);
                    }
                    let stride = image.width.saturating_mul(4) as i32;
                    match cairo::ImageSurface::create_for_data(
                        image.pixels,
                        cairo::Format::ARgb32,
                        image.width as i32,
                        image.height as i32,
                        stride,
                    ) {
                        Ok(surface) => {
                            surface.set_device_scale(scale, scale);
                            state.rendered = Some(surface);
                            if !state.first_frame_logged {
                                focaldesk_logging::flog_info!(
                                    "VUI {role:?} shell-layer client rendered its first frame for connector {draw_connector} ({width}x{height}, scale {scale})"
                                );
                                state.first_frame_logged = true;
                                first_frame_ready.set(true);
                            }
                        }
                        Err(error) => focaldesk_logging::flog_warn!(
                            "cannot create VUI GTK image surface: {error}"
                        ),
                    }
                }
                Err(error) => focaldesk_logging::flog_warn!("VUI shell frame failed: {error}"),
            }
        }
        if let Some(surface) = &state.rendered {
            let _ = context.set_source_surface(surface, 0.0, 0.0);
            let _ = context.paint();
        }
    });

    // Rendered shell controls live in a single GTK DrawingArea; VUI's overlay
    // tooltips would be clipped by that small widget. Ask GTK to create native
    // tooltips instead, using the VUI semantic hit rectangles.
    area.set_has_tooltip(true);
    let tooltip_state = state.clone();
    area.connect_query_tooltip(move |_, x, y, _, tooltip| {
        let point = Point::new(f64::from(x), f64::from(y));
        if let Some((_, label)) = tooltip_state
            .borrow()
            .tooltip_targets
            .iter()
            .find(|(bounds, _)| bounds.contains(point))
        {
            tooltip.set_text(Some(label));
            true
        } else {
            false
        }
    });

    let motion = gtk::EventControllerMotion::new();
    let motion_state = state.clone();
    let motion_area = area.downgrade();
    let motion_actions = actions.clone();
    let motion_pending_hover = pending_dock_hover.clone();
    motion.connect_motion(move |_, x, y| {
        let position = Point::new(x, y);
        if role == ShellRole::Dock {
            *motion_pending_hover.borrow_mut() = Some((position, Instant::now()));
        } else {
            dispatch_event(
                &motion_state,
                &motion_actions,
                &motion_area,
                Event::PointerMoved(position),
            );
        }
    });
    if role == ShellRole::Dock {
        let visibility = dock_visibility.clone();
        let enter_window = window.downgrade();
        motion.connect_enter(move |_, _, _| {
            let mut state = visibility.borrow_mut();
            state.pointer_inside = true;
            state.hide_after = None;
            state.hidden = false;
            if let Some(window) = enter_window.upgrade() {
                window.set_margin(
                    gtk4_layer_shell::Edge::Bottom,
                    crate::TASK_SHELF_VISIBLE_MARGIN,
                );
            }
        });
        let visibility = dock_visibility.clone();
        motion.connect_leave(move |_| {
            let mut state = visibility.borrow_mut();
            state.pointer_inside = false;
            state.hide_after = Some(Instant::now() + crate::TASK_SHELF_HIDE_DELAY);
        });
    }
    let leave_state = state.clone();
    let leave_area = area.downgrade();
    let leave_actions = actions.clone();
    let leave_pending_hover = pending_dock_hover.clone();
    motion.connect_leave(move |_| {
        *leave_pending_hover.borrow_mut() = None;
        dispatch_event(
            &leave_state,
            &leave_actions,
            &leave_area,
            Event::PointerLeft,
        );
    });
    area.add_controller(motion);

    let click = gtk::GestureClick::new();
    click.set_button(0);
    let down_state = state.clone();
    let down_area = area.downgrade();
    let down_actions = actions.clone();
    click.connect_pressed(move |gesture, count, x, y| {
        let button = pointer_button(gesture.current_button());
        dispatch_event(
            &down_state,
            &down_actions,
            &down_area,
            Event::PointerDown {
                position: Point::new(x, y),
                button,
                click_count: count.clamp(1, i32::from(u8::MAX)) as u8,
            },
        );
    });
    let up_state = state.clone();
    let up_area = area.downgrade();
    let up_actions = actions.clone();
    let release_role = role;
    let release_connector = connector.clone();
    click.connect_released(move |gesture, _, x, y| {
        let position = Point::new(x, y);
        let target = up_state
            .borrow()
            .tooltip_targets
            .iter()
            .find(|(bounds, _)| bounds.contains(position))
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| "<no button hit target>".into());
        focaldesk_logging::flog_warn!(
            "VUI GTK pointer release: role={release_role:?} connector={release_connector} x={x:.1} y={y:.1} target={target} button={:?}",
            pointer_button(gesture.current_button())
        );
        dispatch_event(
            &up_state,
            &up_actions,
            &up_area,
            Event::PointerUp {
                position,
                button: pointer_button(gesture.current_button()),
            },
        );
    });
    area.add_controller(click);

    let snapshots = crate::start_snapshot_poll(
        window,
        match role {
            ShellRole::Panel => crate::RAIL_SNAPSHOT_POLL_INTERVAL,
            ShellRole::Dock => crate::BACKGROUND_SNAPSHOT_POLL_INTERVAL,
        },
    );
    let tick_state = state.clone();
    let tick_area = area.downgrade();
    let tick_snapshots = snapshots.clone();
    let tick_latest_snapshot = latest_snapshot.clone();
    let tick_actions = actions.clone();
    let tick_connector = connector.clone();
    let tick_dock_view = dock_view.clone();
    let tick_dock_visibility = dock_visibility.clone();
    let tick_pending_hover = pending_dock_hover.clone();
    let tick_hover_state = state.clone();
    let tick_hover_actions = actions.clone();
    let tick_hover_area = area.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        let Some(area) = tick_area.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let pending_hover = { *tick_pending_hover.borrow() };
        if let Some((position, since)) = pending_hover {
            if since.elapsed() >= DOCK_HOVER_DELAY {
                tick_pending_hover.borrow_mut().take();
                dispatch_event(
                    &tick_hover_state,
                    &tick_hover_actions,
                    &tick_hover_area,
                    Event::PointerMoved(position),
                );
            }
        }
        if let Some(snapshot) = tick_snapshots
            .try_lock()
            .ok()
            .and_then(|mut snapshot| snapshot.take())
        {
            let signature =
                snapshot_signature(role, &tick_connector, &snapshot, &tick_dock_view.borrow());
            let output = crate::output_for_connector(&snapshot, &tick_connector);
            let workspace = output.map_or(snapshot.session.active_workspace_id, |o| {
                o.active_workspace_id
            });
            let obscured = role == ShellRole::Dock
                && output
                    .is_some_and(|output| crate::shelf_is_obscured(&snapshot, output, workspace));
            let dock_signature_changed = {
                let mut visibility = tick_dock_visibility.borrow_mut();
                let should_hide = match crate::load_config().dock.visibility {
                    crate::DockVisibility::AlwaysVisible => false,
                    crate::DockVisibility::IntelligentDodge => obscured,
                    crate::DockVisibility::Autohide => true,
                } && !visibility.pointer_inside
                    && tick_dock_view.borrow().chooser.is_none();
                if !should_hide {
                    visibility.hide_after = None;
                } else {
                    visibility
                        .hide_after
                        .get_or_insert(Instant::now() + crate::TASK_SHELF_HIDE_DELAY);
                }
                let hidden = should_hide
                    && visibility
                        .hide_after
                        .is_some_and(|deadline| Instant::now() >= deadline);
                let changed = hidden != visibility.hidden;
                visibility.hidden = hidden;
                changed
            };
            if dock_signature_changed {
                if let Some(window) = area
                    .root()
                    .and_then(|root| root.downcast::<gtk::ApplicationWindow>().ok())
                {
                    let margin = if tick_dock_visibility.borrow().hidden {
                        -(crate::TASK_SHELF_HEIGHT - crate::TASK_SHELF_REVEAL_STRIP)
                    } else {
                        crate::TASK_SHELF_VISIBLE_MARGIN
                    };
                    window.set_margin(gtk4_layer_shell::Edge::Bottom, margin);
                }
            }
            *tick_latest_snapshot.borrow_mut() = Some(snapshot);
            let mut state = tick_state.borrow_mut();
            if signature != state.snapshot_signature {
                state.runtime = build_runtime(
                    role,
                    &tick_connector,
                    tick_actions.clone(),
                    tick_latest_snapshot.clone(),
                    tick_dock_view.clone(),
                );
                state.snapshot_signature = signature;
                area.queue_draw();
            }
        }
        let mut state = tick_state.borrow_mut();
        let elapsed = state.started.elapsed();
        if state.runtime.advance_time(elapsed) {
            area.queue_draw();
        }
        glib::ControlFlow::Continue
    });

    window.set_child(Some(&area));
    area.grab_focus();
    focaldesk_logging::flog_warn!(
        "VUI shell-layer client selected: role={role:?} connector={connector} renderer=Vello"
    );
    Some(area)
}

fn dispatch_event(
    state: &Rc<RefCell<ClientState>>,
    actions: &Rc<RefCell<Vec<DesktopAction>>>,
    area: &glib::WeakRef<gtk::DrawingArea>,
    event: Event,
) {
    state.borrow_mut().runtime.handle_event(&event);
    for action in std::mem::take(&mut *actions.borrow_mut()) {
        focaldesk_logging::flog_warn!("VUI GTK shell action queued: {action:?}");
        std::thread::spawn(move || {
            match send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                action: action.clone(),
            }) {
                Ok(IpcResponse::Ok) => {
                    focaldesk_logging::flog_info!("VUI shell action accepted: {action:?}");
                }
                Ok(response) => {
                    focaldesk_logging::flog_warn!(
                        "VUI shell action {action:?} returned unexpected response: {response:?}"
                    );
                }
                Err(error) => {
                    focaldesk_logging::flog_warn!(
                        "VUI shell action {action:?} could not reach the compositor: {error}"
                    );
                }
            }
        });
    }
    if let Some(area) = area.upgrade() {
        area.queue_draw();
    }
}

fn pointer_button(button: u32) -> PointerButton {
    match button {
        3 => PointerButton::Secondary,
        2 => PointerButton::Middle,
        _ => PointerButton::Primary,
    }
}

fn button(
    label: &str,
    action: DesktopAction,
    actions: Rc<RefCell<Vec<DesktopAction>>>,
    selected: bool,
    disabled: bool,
) -> Box<dyn Widget> {
    if let Some(icon) = button_icon(label) {
        let accessible_label = button_accessible_label(label);
        let click_actions = actions.clone();
        let click_action = action.clone();
        let icon_button = IconButton::data(icon, accessible_label, move || {
            click_actions.borrow_mut().push(click_action.clone());
        })
        .disabled(disabled);
        let control = Surface::new(icon_button)
            .background(if selected {
                vui_core::Color::rgba(52, 118, 168, 110)
            } else {
                vui_core::Color::rgba(74, 88, 110, 26)
            })
            .border(
                if selected {
                    vui_core::Color::rgba(102, 190, 244, 190)
                } else {
                    vui_core::Color::rgba(112, 132, 156, 70)
                },
                1.0,
            )
            .radius(14.0);
        return Box::new(vui_core::SizedBox::new(control).width(44.0).height(44.0));
    }
    let widget = Button::new(label, move || actions.borrow_mut().push(action.clone()))
        .variant(ButtonVariant::Icon)
        .selected(selected)
        .disabled(disabled);
    Box::new(vui_core::SizedBox::new(widget).width(44.0).height(44.0))
}

fn button_accessible_label(label: &str) -> String {
    match label {
        "AI" => "FocalDesk AI Console".into(),
        "QS" => "Quick Settings".into(),
        "Wi" => "Network".into(),
        "Au" => "Audio".into(),
        "BT" => "Bluetooth".into(),
        "DP" => "Display settings".into(),
        "HDR" => "Display settings — HDR active".into(),
        "!" => "Display settings — attention required".into(),
        "Mic" => "Microphone in use — audio settings".into(),
        "Cam" => "Camera in use — privacy settings".into(),
        "Cl" => "Calendar and clock".into(),
        "Nt" => "Notification center".into(),
        "Up" => "System updates".into(),
        "Pw" | "Pwr" => "Battery and power".into(),
        "Ma" => "Email".into(),
        "Br" => "Browser".into(),
        "Te" => "Terminal".into(),
        "Fi" => "Files".into(),
        "Ap" => "Application launcher".into(),
        "Se" => "Settings".into(),
        value if value.ends_with('%') => "Battery charge and power".into(),
        other => other.to_owned(),
    }
}

fn button_icon(label: &str) -> Option<IconData> {
    use PathCommand::{CurveTo, LineTo, MoveTo};
    let point = Point::new;
    let commands = match label {
        "AI" => return Some(IconName::Command.data()),
        "QS" | "Se" => return Some(IconName::Settings.data()),
        "Wi" => vec![
            MoveTo(point(4.0, 9.0)),
            CurveTo(point(8.5, 5.0), point(15.5, 5.0), point(20.0, 9.0)),
            MoveTo(point(7.0, 13.0)),
            CurveTo(point(10.0, 10.5), point(14.0, 10.5), point(17.0, 13.0)),
            MoveTo(point(10.0, 17.0)),
            CurveTo(point(11.3, 16.0), point(12.7, 16.0), point(14.0, 17.0)),
            MoveTo(point(12.0, 20.0)),
            LineTo(point(12.0, 20.2)),
        ],
        "Au" => vec![
            MoveTo(point(4.0, 10.0)),
            LineTo(point(8.0, 10.0)),
            LineTo(point(13.0, 6.0)),
            LineTo(point(13.0, 18.0)),
            LineTo(point(8.0, 14.0)),
            LineTo(point(4.0, 14.0)),
            LineTo(point(4.0, 10.0)),
            MoveTo(point(16.0, 9.0)),
            CurveTo(point(18.5, 10.5), point(18.5, 13.5), point(16.0, 15.0)),
        ],
        "BT" => vec![
            MoveTo(point(8.0, 7.0)),
            LineTo(point(17.0, 17.0)),
            LineTo(point(12.0, 21.0)),
            LineTo(point(12.0, 3.0)),
            LineTo(point(17.0, 7.0)),
            LineTo(point(8.0, 17.0)),
        ],
        "DP" | "HDR" | "!" => vec![
            MoveTo(point(4.0, 5.0)),
            LineTo(point(20.0, 5.0)),
            LineTo(point(20.0, 16.0)),
            LineTo(point(4.0, 16.0)),
            LineTo(point(4.0, 5.0)),
            MoveTo(point(9.0, 20.0)),
            LineTo(point(15.0, 20.0)),
            MoveTo(point(12.0, 16.0)),
            LineTo(point(12.0, 20.0)),
        ],
        "Mic" => vec![
            MoveTo(point(12.0, 3.0)),
            CurveTo(point(15.0, 3.0), point(15.0, 5.0), point(15.0, 8.0)),
            LineTo(point(15.0, 12.0)),
            CurveTo(point(15.0, 16.0), point(9.0, 16.0), point(9.0, 12.0)),
            LineTo(point(9.0, 8.0)),
            CurveTo(point(9.0, 5.0), point(9.0, 3.0), point(12.0, 3.0)),
            MoveTo(point(5.0, 11.0)),
            CurveTo(point(5.0, 20.0), point(19.0, 20.0), point(19.0, 11.0)),
            MoveTo(point(12.0, 17.0)),
            LineTo(point(12.0, 21.0)),
        ],
        "Cam" => vec![
            MoveTo(point(4.0, 7.0)),
            LineTo(point(14.0, 7.0)),
            LineTo(point(14.0, 17.0)),
            LineTo(point(4.0, 17.0)),
            LineTo(point(4.0, 7.0)),
            MoveTo(point(14.0, 10.0)),
            LineTo(point(20.0, 7.0)),
            LineTo(point(20.0, 17.0)),
            LineTo(point(14.0, 14.0)),
        ],
        "Cl" => vec![
            MoveTo(point(12.0, 3.0)),
            CurveTo(point(17.0, 3.0), point(21.0, 7.0), point(21.0, 12.0)),
            CurveTo(point(21.0, 17.0), point(17.0, 21.0), point(12.0, 21.0)),
            CurveTo(point(7.0, 21.0), point(3.0, 17.0), point(3.0, 12.0)),
            CurveTo(point(3.0, 7.0), point(7.0, 3.0), point(12.0, 3.0)),
            MoveTo(point(12.0, 7.0)),
            LineTo(point(12.0, 12.0)),
            LineTo(point(16.0, 14.0)),
        ],
        "Nt" => vec![
            MoveTo(point(5.0, 17.0)),
            LineTo(point(7.0, 15.0)),
            LineTo(point(7.0, 10.0)),
            CurveTo(point(7.0, 3.5), point(17.0, 3.5), point(17.0, 10.0)),
            LineTo(point(17.0, 15.0)),
            LineTo(point(19.0, 17.0)),
            LineTo(point(5.0, 17.0)),
            MoveTo(point(10.0, 20.0)),
            CurveTo(point(11.0, 21.0), point(13.0, 21.0), point(14.0, 20.0)),
        ],
        "Up" => vec![
            MoveTo(point(5.0, 9.0)),
            CurveTo(point(7.0, 4.0), point(14.0, 2.5), point(18.0, 6.0)),
            LineTo(point(20.0, 8.0)),
            MoveTo(point(20.0, 4.0)),
            LineTo(point(20.0, 8.0)),
            LineTo(point(16.0, 8.0)),
            MoveTo(point(19.0, 15.0)),
            CurveTo(point(17.0, 20.0), point(10.0, 21.5), point(6.0, 18.0)),
            LineTo(point(4.0, 16.0)),
            MoveTo(point(4.0, 20.0)),
            LineTo(point(4.0, 16.0)),
            LineTo(point(8.0, 16.0)),
        ],
        "Pw" | "Pwr" => vec![
            MoveTo(point(12.0, 3.0)),
            LineTo(point(12.0, 12.0)),
            MoveTo(point(7.5, 6.0)),
            CurveTo(point(1.5, 10.5), point(4.5, 20.5), point(12.0, 21.0)),
            CurveTo(point(19.5, 20.5), point(22.5, 10.5), point(16.5, 6.0)),
        ],
        "Br" => return Some(app_icon(AppIcon::Browser)),
        "Te" => return Some(app_icon(AppIcon::Terminal)),
        "Fi" => return Some(app_icon(AppIcon::Files)),
        "Ma" => return Some(app_icon(AppIcon::Mail)),
        "Ap" => return Some(app_icon(AppIcon::Applications)),
        value if value.ends_with('%') => return Some(app_icon(AppIcon::Battery)),
        value if value.len() == 2 && value.chars().all(|ch| ch.is_ascii_uppercase()) => {
            return Some(app_icon(AppIcon::Generic));
        }
        _ => return None,
    };
    Some(IconData::new(Size::new(24.0, 24.0), commands))
}

#[derive(Clone, Copy)]
enum AppIcon {
    Browser,
    Terminal,
    Files,
    Mail,
    Applications,
    Battery,
    Generic,
}

fn app_icon_for_identity(identity: &str) -> AppIcon {
    let identity = identity.to_ascii_lowercase();
    if identity.contains("chrome") || identity.contains("firefox") || identity.contains("browser") {
        AppIcon::Browser
    } else if identity.contains("terminal") || identity.contains("foot") {
        AppIcon::Terminal
    } else if identity.contains("file") || identity.contains("nautilus") {
        AppIcon::Files
    } else if identity.contains("mail")
        || identity.contains("evolution")
        || identity.contains("thunderbird")
    {
        AppIcon::Mail
    } else {
        AppIcon::Generic
    }
}

fn app_icon(icon: AppIcon) -> IconData {
    use PathCommand::{CurveTo, LineTo, MoveTo};
    let p = Point::new;
    let commands = match icon {
        AppIcon::Browser => vec![
            MoveTo(p(12.0, 3.0)),
            CurveTo(p(17.0, 3.0), p(21.0, 7.0), p(21.0, 12.0)),
            CurveTo(p(21.0, 17.0), p(17.0, 21.0), p(12.0, 21.0)),
            CurveTo(p(7.0, 21.0), p(3.0, 17.0), p(3.0, 12.0)),
            CurveTo(p(3.0, 7.0), p(7.0, 3.0), p(12.0, 3.0)),
            MoveTo(p(3.5, 12.0)),
            LineTo(p(20.5, 12.0)),
            MoveTo(p(12.0, 3.5)),
            CurveTo(p(8.5, 7.0), p(8.5, 17.0), p(12.0, 20.5)),
            MoveTo(p(12.0, 3.5)),
            CurveTo(p(15.5, 7.0), p(15.5, 17.0), p(12.0, 20.5)),
        ],
        AppIcon::Terminal => vec![
            MoveTo(p(4.0, 5.0)),
            LineTo(p(20.0, 5.0)),
            LineTo(p(20.0, 19.0)),
            LineTo(p(4.0, 19.0)),
            LineTo(p(4.0, 5.0)),
            MoveTo(p(7.0, 9.0)),
            LineTo(p(10.0, 12.0)),
            LineTo(p(7.0, 15.0)),
            MoveTo(p(12.0, 15.0)),
            LineTo(p(17.0, 15.0)),
        ],
        AppIcon::Files => vec![
            MoveTo(p(3.0, 7.0)),
            LineTo(p(10.0, 7.0)),
            LineTo(p(12.0, 9.0)),
            LineTo(p(21.0, 9.0)),
            LineTo(p(21.0, 19.0)),
            LineTo(p(3.0, 19.0)),
            LineTo(p(3.0, 7.0)),
        ],
        AppIcon::Mail => vec![
            MoveTo(p(3.0, 6.0)),
            LineTo(p(21.0, 6.0)),
            LineTo(p(21.0, 18.0)),
            LineTo(p(3.0, 18.0)),
            LineTo(p(3.0, 6.0)),
            MoveTo(p(3.5, 7.0)),
            LineTo(p(12.0, 13.0)),
            LineTo(p(20.5, 7.0)),
        ],
        AppIcon::Applications => vec![
            MoveTo(p(5.0, 5.0)),
            LineTo(p(9.0, 5.0)),
            LineTo(p(9.0, 9.0)),
            LineTo(p(5.0, 9.0)),
            LineTo(p(5.0, 5.0)),
            MoveTo(p(15.0, 5.0)),
            LineTo(p(19.0, 5.0)),
            LineTo(p(19.0, 9.0)),
            LineTo(p(15.0, 9.0)),
            LineTo(p(15.0, 5.0)),
            MoveTo(p(5.0, 15.0)),
            LineTo(p(9.0, 15.0)),
            LineTo(p(9.0, 19.0)),
            LineTo(p(5.0, 19.0)),
            LineTo(p(5.0, 15.0)),
            MoveTo(p(15.0, 15.0)),
            LineTo(p(19.0, 15.0)),
            LineTo(p(19.0, 19.0)),
            LineTo(p(15.0, 19.0)),
            LineTo(p(15.0, 15.0)),
        ],
        AppIcon::Battery => vec![
            MoveTo(p(6.0, 6.0)),
            LineTo(p(18.0, 6.0)),
            LineTo(p(18.0, 18.0)),
            LineTo(p(6.0, 18.0)),
            LineTo(p(6.0, 6.0)),
            MoveTo(p(9.0, 4.0)),
            LineTo(p(15.0, 4.0)),
            MoveTo(p(9.0, 9.0)),
            LineTo(p(9.0, 15.0)),
            LineTo(p(15.0, 15.0)),
        ],
        AppIcon::Generic => vec![
            MoveTo(p(4.0, 5.0)),
            LineTo(p(20.0, 5.0)),
            LineTo(p(20.0, 19.0)),
            LineTo(p(4.0, 19.0)),
            LineTo(p(4.0, 5.0)),
            MoveTo(p(4.0, 9.0)),
            LineTo(p(20.0, 9.0)),
        ],
    };
    IconData::new(Size::new(24.0, 24.0), commands)
}

fn open_panel(connector: &str, panel: ShellPanel) -> DesktopAction {
    DesktopAction::OpenShellPanel {
        connector: connector.to_owned(),
        panel,
    }
}

fn build_runtime(
    role: ShellRole,
    connector: &str,
    actions: Rc<RefCell<Vec<DesktopAction>>>,
    latest_snapshot: Rc<RefCell<Option<DesktopSnapshot>>>,
    dock_view: Rc<RefCell<DockViewState>>,
) -> UiRuntime {
    let root = Column::new([match role {
        ShellRole::Panel => rail_root(actions, connector, latest_snapshot),
        ShellRole::Dock => dock_root(actions, connector, latest_snapshot, dock_view),
    }]);
    let text_system = ParleyTextSystem::with_embedded_fonts([
        include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf").as_slice(),
    ])
    .unwrap_or_else(|error| {
        focaldesk_logging::flog_warn!(
            "cannot initialize embedded IBM Plex Sans for VUI shell: {error}; using system font fallback"
        );
        ParleyTextSystem::new()
    });
    let mut runtime = UiRuntime::new(root).with_text_system(text_system);
    runtime.set_theme(Theme::dark());
    runtime.set_transparent(true);
    runtime.request_redraw();
    runtime
}

fn rail_root(
    actions: Rc<RefCell<Vec<DesktopAction>>>,
    connector: &str,
    latest_snapshot: Rc<RefCell<Option<DesktopSnapshot>>>,
) -> Box<dyn Widget> {
    let mut buttons = Vec::<Box<dyn Widget>>::new();
    let snapshot = latest_snapshot.borrow();
    let output = snapshot
        .as_ref()
        .and_then(|snapshot| crate::output_for_connector(snapshot, connector));
    let output_focused = output.is_some_and(|output| output.focused);
    let output_hdr_active = output.is_some_and(|output| output.hdr_active);
    let active_workspace = output
        .map(|output| output.active_workspace_id)
        .or_else(|| {
            snapshot
                .as_ref()
                .map(|snapshot| snapshot.session.active_workspace_id)
        })
        .unwrap_or(1)
        .max(1);
    let workspace_count = snapshot
        .as_ref()
        .map(|snapshot| snapshot.shell.workspace_count.max(1).min(9))
        .unwrap_or(4);
    let max_workspace_slots = snapshot
        .as_ref()
        .map(|snapshot| snapshot.shell.max_workspace_slots.max(1))
        .unwrap_or(9);
    let network_active = snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.shell.network_carrier);
    let notification_active = snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.shell.notification_unread_count > 0);
    let updates_active = snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.shell.update_busy || snapshot.shell.update_available_count > 0
    });
    let ai_target = snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .windows
            .iter()
            .find(|window| ai_window_matches(window) && !window.minimized)
            .map(|window| window.id)
    });
    let ai_latest = latest_snapshot.clone();
    let ai_action = DesktopAction::LaunchApp {
        app: "@ai-console".into(),
    };
    let ai_actions = actions.clone();
    let ai_widget = IconButton::new(IconName::Command, "FocalDesk AI Console", move || {
        let target = ai_latest.borrow().as_ref().and_then(|snapshot| {
            snapshot
                .windows
                .iter()
                .find(|window| ai_window_matches(window) && !window.minimized)
                .map(|window| window.id)
        });
        ai_actions.borrow_mut().push(target.map_or_else(
            || ai_action.clone(),
            |window_id| DesktopAction::FocusWindow { window_id },
        ));
    });
    buttons.push(Box::new(
        vui_core::SizedBox::new(
            Surface::new(ai_widget)
                .background(if ai_target.is_some() {
                    vui_core::Color::rgba(52, 118, 168, 110)
                } else {
                    vui_core::Color::rgba(74, 88, 110, 26)
                })
                .border(
                    if ai_target.is_some() {
                        vui_core::Color::rgba(102, 190, 244, 190)
                    } else {
                        vui_core::Color::rgba(112, 132, 156, 70)
                    },
                    1.0,
                )
                .radius(14.0),
        )
        .width(44.0)
        .height(44.0),
    ));
    for (label, panel) in [
        ("QS", ShellPanel::QuickSettings),
        ("Wi", ShellPanel::Network),
        ("Au", ShellPanel::Audio),
        ("BT", ShellPanel::Bluetooth),
    ] {
        buttons.push(button(
            label,
            open_panel(&connector, panel),
            actions.clone(),
            label == "Wi" && network_active,
            false,
        ));
    }
    let display_status = output.map(crate::rail_display_status);
    let display_label = display_status
        .as_ref()
        .and_then(|status| status.badge)
        .unwrap_or("DP");
    let display_control = button(
        if output_hdr_active {
            "HDR"
        } else {
            display_label
        },
        open_panel(connector, ShellPanel::Display),
        actions.clone(),
        output_hdr_active,
        false,
    );
    buttons.push(if output_hdr_active {
        hdr_badged_control(display_control)
    } else {
        display_control
    });
    if let Some(snapshot) = snapshot.as_ref() {
        if snapshot.shell.microphone_active {
            buttons.push(button(
                "Mic",
                open_panel(connector, ShellPanel::Audio),
                actions.clone(),
                true,
                false,
            ));
        }
        if snapshot.shell.camera_active {
            buttons.push(button(
                "Cam",
                open_panel(connector, ShellPanel::Settings),
                actions.clone(),
                true,
                false,
            ));
        }
        let label = snapshot
            .shell
            .battery_percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_else(|| "Pwr".into());
        buttons.push(button(
            &label,
            open_panel(connector, ShellPanel::Power),
            actions.clone(),
            snapshot.shell.battery_charging,
            false,
        ));
    }
    for workspace in 1..=workspace_count {
        buttons.push(rail_text_button(
            &workspace.to_string(),
            &format!("Workspace {workspace}"),
            DesktopAction::FocusWorkspaceOnOutput {
                connector: connector.to_owned(),
                workspace: workspace as u32,
            },
            actions.clone(),
            workspace as u32 == active_workspace,
            output_hdr_active,
            false,
        ));
    }
    buttons.push(rail_text_button(
        "+",
        "Create workspace",
        DesktopAction::CreateWorkspaceOnOutput {
            connector: connector.to_owned(),
        },
        actions.clone(),
        false,
        output_hdr_active,
        workspace_count >= max_workspace_slots,
    ));
    if workspace_count > 1 {
        buttons.push(rail_text_button(
            "−",
            "Delete workspace",
            DesktopAction::DeleteWorkspaceOnOutput {
                connector: connector.to_owned(),
            },
            actions.clone(),
            false,
            output_hdr_active,
            false,
        ));
    }
    for (label, panel) in [
        ("Nt", ShellPanel::NotificationHistory),
        ("Up", ShellPanel::Updates),
        ("Pw", ShellPanel::Power),
    ] {
        buttons.push(button(
            label,
            open_panel(&connector, panel),
            actions.clone(),
            (label == "Nt" && notification_active) || (label == "Up" && updates_active),
            false,
        ));
    }
    let clock = chrono::Local::now()
        .format(crate::rail_clock_pattern(
            crate::load_config().panel.clock_format,
        ))
        .to_string();
    buttons.insert(
        buttons.len().saturating_sub(3),
        button(
            &clock,
            open_panel(connector, ShellPanel::Calendar),
            actions.clone(),
            false,
            false,
        ),
    );
    let rail = Surface::new(Column::new(buttons).gap(4.0))
        .padding(Insets::all(6.0))
        .background(vui_core::Color::rgba(20, 24, 32, 230))
        .border(vui_core::Color::rgba(112, 128, 150, 194), 1.0)
        .radius(18.0);
    let marker = Surface::new(Spacer::new(24.0, 3.0))
        .padding(Insets::all(0.0))
        .background(if output_focused {
            vui_core::Color::rgba(102, 205, 255, 245)
        } else {
            vui_core::Color::rgba(112, 128, 150, 70)
        })
        .radius(2.0);
    let root = Flex::column([
        FlexChild::fixed(marker),
        FlexChild::fixed(rail),
        FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
    ])
    .cross_alignment(vui_core::CrossAxisAlignment::Center);
    Box::new(vui_core::SizedBox::new(root).fill_width().fill_height())
}

/// Draws the label without adding a duplicate accessibility node; the
/// overlaid IconButton owns the hit target and button semantics.
struct VisualOnlyLabel(Label);

impl Widget for VisualOnlyLabel {
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, constraints: BoxConstraints) -> Size {
        self.0.layout(ctx, constraints)
    }

    fn set_origin(&mut self, origin: Point) {
        self.0.set_origin(origin);
    }

    fn bounds(&self) -> Rect {
        self.0.bounds()
    }

    fn event(&mut self, _ctx: &mut EventCtx, _event: &Event) {}

    fn paint(&self, ctx: &mut PaintCtx<'_>) {
        self.0.paint(ctx);
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx<'_>) {}
}

fn hdr_badged_control(control: Box<dyn Widget>) -> Box<dyn Widget> {
    let badge_label = VisualOnlyLabel(Label::new("HDR").text_size(7.0));
    let badge = Surface::new(
        Stack::new([Box::new(badge_label) as Box<dyn Widget>])
            .horizontal_alignment(CrossAxisAlignment::Center)
            .vertical_alignment(CrossAxisAlignment::Center),
    )
    .background(vui_core::Color::rgba(20, 45, 62, 255))
    .border(vui_core::Color::rgba(111, 215, 255, 255), 1.0)
    .radius(4.0);
    let badge = vui_core::SizedBox::new(badge).width(22.0).height(11.0);
    let marked = Stack::new([control, Box::new(badge) as Box<dyn Widget>])
        .horizontal_alignment(CrossAxisAlignment::End)
        .vertical_alignment(CrossAxisAlignment::Start);
    Box::new(vui_core::SizedBox::new(marked).width(44.0).height(44.0))
}

fn rail_text_button(
    label: &str,
    accessible_label: &str,
    action: DesktopAction,
    actions: Rc<RefCell<Vec<DesktopAction>>>,
    selected: bool,
    hdr_active: bool,
    disabled: bool,
) -> Box<dyn Widget> {
    let click_actions = actions.clone();
    let click_action = action.clone();
    let click_target = IconButton::data(
        IconData::new(Size::new(24.0, 24.0), Vec::new()),
        accessible_label,
        move || click_actions.borrow_mut().push(click_action.clone()),
    )
    .disabled(disabled);
    let centered = Stack::new([
        Box::new(click_target) as Box<dyn Widget>,
        Box::new(VisualOnlyLabel(Label::new(label).text_size(16.0))),
    ])
    .horizontal_alignment(CrossAxisAlignment::Center)
    .vertical_alignment(CrossAxisAlignment::Center);
    let control = Surface::new(centered)
        .background(if selected {
            vui_core::Color::rgba(52, 118, 168, 110)
        } else {
            vui_core::Color::rgba(74, 88, 110, 26)
        })
        .border(
            if selected && hdr_active {
                vui_core::Color::rgba(126, 224, 255, 255)
            } else if selected {
                vui_core::Color::rgba(102, 190, 244, 190)
            } else {
                vui_core::Color::rgba(112, 132, 156, 70)
            },
            if selected && hdr_active { 1.5 } else { 1.0 },
        )
        .radius(if selected && hdr_active { 10.0 } else { 12.0 });
    if selected && hdr_active {
        let halo = Surface::new(Spacer::new(44.0, 44.0))
            .background(vui_core::Color::rgba(48, 178, 255, 36))
            .border(vui_core::Color::rgba(76, 196, 255, 130), 1.0)
            .radius(14.0);
        let layered = Stack::new([
            Box::new(halo) as Box<dyn Widget>,
            Box::new(vui_core::SizedBox::new(control).width(40.0).height(40.0)),
        ])
        .horizontal_alignment(CrossAxisAlignment::Center)
        .vertical_alignment(CrossAxisAlignment::Center);
        Box::new(vui_core::SizedBox::new(layered).width(44.0).height(44.0))
    } else {
        Box::new(vui_core::SizedBox::new(control).width(44.0).height(44.0))
    }
}

fn dock_root(
    actions: Rc<RefCell<Vec<DesktopAction>>>,
    connector: &str,
    latest_snapshot: Rc<RefCell<Option<DesktopSnapshot>>>,
    dock_view: Rc<RefCell<DockViewState>>,
) -> Box<dyn Widget> {
    if let Some(windows) = dock_view.borrow().chooser.clone() {
        let page = dock_view.borrow().page;
        let start = page.saturating_mul(7).min(windows.len());
        let end = start.saturating_add(7).min(windows.len());
        let view = dock_view.clone();
        let close = IconButton::new(IconName::Close, "Close window chooser", move || {
            let mut view = view.borrow_mut();
            view.chooser = None;
            view.page = 0;
        });
        let mut choices: Vec<Box<dyn Widget>> = vec![Box::new(
            vui_core::SizedBox::new(close).width(44.0).height(44.0),
        )];
        for window in windows.iter().skip(start).take(end - start) {
            let title = if window.title.trim().is_empty() {
                "Window"
            } else {
                window.title.trim()
            };
            let title = title.chars().take(7).collect::<String>();
            let window_id = window.id;
            let focused = window.focused;
            let view = dock_view.clone();
            let choices_actions = actions.clone();
            let choice = IconButton::data(app_icon(AppIcon::Generic), title.clone(), move || {
                let mut view = view.borrow_mut();
                view.chooser = None;
                view.page = 0;
                drop(view);
                choices_actions
                    .borrow_mut()
                    .push(DesktopAction::FocusWindow { window_id });
            })
            .disabled(false);
            choices.push(Box::new(
                vui_core::SizedBox::new(
                    Surface::new(choice)
                        .background(if focused {
                            vui_core::Color::rgba(52, 118, 168, 110)
                        } else {
                            vui_core::Color::rgba(74, 88, 110, 26)
                        })
                        .border(
                            if focused {
                                vui_core::Color::rgba(102, 190, 244, 190)
                            } else {
                                vui_core::Color::rgba(112, 132, 156, 70)
                            },
                            1.0,
                        )
                        .radius(14.0),
                )
                .width(44.0)
                .height(44.0),
            ));
        }
        if end < windows.len() {
            let view = dock_view.clone();
            let next =
                Button::new("›", move || view.borrow_mut().page += 1).variant(ButtonVariant::Icon);
            choices.push(Box::new(
                vui_core::SizedBox::new(next).width(44.0).height(44.0),
            ));
        }
        let panel = Surface::new(Flex::row(choices.into_iter().map(FlexChild::from)).gap(4.0))
            .padding(Insets::all(6.0))
            .background(vui_core::Color::rgba(20, 24, 32, 240))
            .border(vui_core::Color::rgba(112, 128, 150, 194), 1.0)
            .radius(16.0);
        let centered = Flex::row([
            FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
            FlexChild::fixed(panel),
            FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
        ])
        .cross_alignment(vui_core::CrossAxisAlignment::Center);
        return Box::new(vui_core::SizedBox::new(centered).fill_width().fill_height());
    }
    let apps = [("Br", "@browser"), ("Te", "@terminal"), ("Fi", "@files")];
    let mut buttons = apps
        .into_iter()
        .map(|(label, app)| {
            button(
                label,
                DesktopAction::LaunchApp { app: app.into() },
                actions.clone(),
                false,
                false,
            )
        })
        .collect::<Vec<_>>();
    let email_command = crate::preferred_email_command();
    if let Some(email) = email_command.clone() {
        let email_latest = latest_snapshot.clone();
        let email_actions = actions.clone();
        let email_action = DesktopAction::LaunchApp { app: email };
        let email_button = IconButton::data(app_icon(AppIcon::Mail), "Email", move || {
            let target = email_latest.borrow().as_ref().and_then(|snapshot| {
                crate::existing_email_window(snapshot, email_command.as_deref()?)
            });
            email_actions.borrow_mut().push(target.map_or_else(
                || email_action.clone(),
                |window_id| DesktopAction::FocusWindow { window_id },
            ));
        });
        let email_control = Surface::new(email_button)
            .background(vui_core::Color::rgba(74, 88, 110, 26))
            .border(vui_core::Color::rgba(112, 132, 156, 70), 1.0)
            .radius(14.0);
        buttons.insert(
            3,
            Box::new(
                vui_core::SizedBox::new(email_control)
                    .width(44.0)
                    .height(44.0),
            ),
        );
    }
    let snapshot = latest_snapshot.borrow();
    if let Some(snapshot) = snapshot.as_ref() {
        let output = crate::output_for_connector(snapshot, connector);
        let workspace = output
            .map(|output| output.active_workspace_id)
            .unwrap_or(snapshot.session.active_workspace_id);
        let mut groups: Vec<(String, Vec<&WindowSnapshot>)> = Vec::new();
        for window in snapshot.windows.iter().filter(|window| {
            crate::window_belongs_to_output(window, output, snapshot.outputs.len())
                && window.workspace_id == workspace
                && window.mapped
                && !window.minimized
        }) {
            let identity = window
                .app_id
                .as_deref()
                .or(window.class.as_deref())
                .unwrap_or(&window.title)
                .to_ascii_lowercase();
            if let Some((_, windows)) = groups.iter_mut().find(|(key, _)| *key == identity) {
                windows.push(window);
            } else {
                groups.push((identity, vec![window]));
            }
        }
        let capacity = 4usize;
        let has_overflow = groups.len() > capacity;
        let overflow_windows = groups
            .iter()
            .flat_map(|(_, windows)| windows.iter().map(|window| (*window).clone()))
            .collect::<Vec<_>>();
        for (identity, windows) in groups.into_iter().take(capacity) {
            let target = windows
                .iter()
                .find(|window| window.focused)
                .copied()
                .unwrap_or(windows[0]);
            if windows.len() > 1 {
                let view = dock_view.clone();
                let grouped = windows
                    .iter()
                    .map(|window| (*window).clone())
                    .collect::<Vec<_>>();
                let grouped_button = IconButton::data(
                    app_icon(app_icon_for_identity(&identity)),
                    target.title.clone(),
                    move || {
                        let mut view = view.borrow_mut();
                        view.chooser = Some(grouped.clone());
                        view.page = 0;
                    },
                );
                let focused = windows.iter().any(|window| window.focused);
                buttons.push(Box::new(
                    vui_core::SizedBox::new(
                        Surface::new(grouped_button)
                            .background(if focused {
                                vui_core::Color::rgba(52, 118, 168, 110)
                            } else {
                                vui_core::Color::rgba(74, 88, 110, 26)
                            })
                            .border(
                                if focused {
                                    vui_core::Color::rgba(102, 190, 244, 190)
                                } else {
                                    vui_core::Color::rgba(112, 132, 156, 70)
                                },
                                1.0,
                            )
                            .radius(14.0),
                    )
                    .width(44.0)
                    .height(44.0),
                ));
            } else {
                buttons.push(app_window_button(
                    &identity,
                    &target.title,
                    target.id,
                    target.focused,
                    actions.clone(),
                ));
            }
        }
        if has_overflow {
            let view = dock_view.clone();
            let overflow_button = Button::new("+", move || {
                let mut view = view.borrow_mut();
                view.chooser = Some(overflow_windows.clone());
                view.page = 0;
            })
            .variant(ButtonVariant::Icon);
            buttons.push(Box::new(
                vui_core::SizedBox::new(overflow_button)
                    .width(44.0)
                    .height(44.0),
            ));
        }
    }
    drop(snapshot);
    buttons.push(button(
        "Ap",
        DesktopAction::LaunchApp {
            app: "@launcher".into(),
        },
        actions.clone(),
        false,
        false,
    ));
    buttons.push(button(
        "Se",
        DesktopAction::LaunchApp {
            app: "@settings".into(),
        },
        actions.clone(),
        false,
        false,
    ));
    let buttons = buttons.into_iter();
    let shelf = Surface::new(Flex::row(buttons.map(FlexChild::from)).gap(4.0))
        .padding(Insets {
            left: 8.0,
            top: 6.0,
            right: 8.0,
            bottom: 6.0,
        })
        .background(vui_core::Color::rgba(20, 24, 32, 230))
        .border(vui_core::Color::rgba(112, 128, 150, 194), 1.0)
        .radius(16.0);
    let centered = Flex::row([
        FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
        FlexChild::fixed(shelf),
        FlexChild::flexible(1.0, Spacer::new(0.0, 0.0)),
    ])
    .cross_alignment(vui_core::CrossAxisAlignment::Center);
    Box::new(vui_core::SizedBox::new(centered).fill_width().fill_height())
}

fn ai_window_matches(window: &WindowSnapshot) -> bool {
    crate::ai_console_identity_matches(
        window.app_id.as_deref(),
        window.class.as_deref(),
        &window.title,
    )
}

fn app_window_button(
    identity: &str,
    title: &str,
    window_id: u32,
    focused: bool,
    actions: Rc<RefCell<Vec<DesktopAction>>>,
) -> Box<dyn Widget> {
    let label = if title.trim().is_empty() {
        identity
    } else {
        title
    };
    let click_actions = actions.clone();
    let button = IconButton::data(
        app_icon(app_icon_for_identity(identity)),
        label.to_owned(),
        move || {
            click_actions
                .borrow_mut()
                .push(DesktopAction::FocusWindow { window_id });
        },
    );
    let control = Surface::new(button)
        .background(if focused {
            vui_core::Color::rgba(52, 118, 168, 110)
        } else {
            vui_core::Color::rgba(74, 88, 110, 26)
        })
        .border(
            if focused {
                vui_core::Color::rgba(102, 190, 244, 190)
            } else {
                vui_core::Color::rgba(112, 132, 156, 70)
            },
            1.0,
        )
        .radius(14.0);
    Box::new(vui_core::SizedBox::new(control).width(44.0).height(44.0))
}

fn snapshot_signature(
    role: ShellRole,
    connector: &str,
    snapshot: &DesktopSnapshot,
    view: &DockViewState,
) -> Vec<String> {
    let output = crate::output_for_connector(snapshot, connector);
    let mut signature = vec![format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        snapshot.shell.workspace_count,
        snapshot.shell.max_workspace_slots,
        snapshot.shell.network_carrier,
        snapshot.shell.notification_unread_count,
        snapshot.shell.update_available_count,
        snapshot.shell.update_busy,
        snapshot.shell.microphone_detected,
        snapshot.shell.microphone_active,
        snapshot.shell.camera_detected,
        snapshot.shell.camera_active,
        snapshot.shell.battery_percent.map_or(-1, i16::from),
        snapshot.session.active_workspace_id,
        output.map_or(0, |output| output.active_workspace_id),
        output.map_or(0, |output| output.id),
    )];
    signature.push(format!(
        "{}:{}:{}:{}:{}",
        output.is_some_and(|output| output.focused),
        output.is_some_and(|output| output.hdr_active),
        output.is_some_and(|output| output.hdr_requested),
        output.is_some_and(|output| output.wide_gamut_active),
        output.is_some_and(|output| output.icc_lut_fallback_active),
    ));
    if role == ShellRole::Dock {
        signature.push(crate::preferred_email_command().unwrap_or_default());
        signature.push(
            view.chooser
                .as_ref()
                .map(|windows| {
                    windows
                        .iter()
                        .map(|window| window.id.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default(),
        );
        signature.push(view.page.to_string());
        let workspace = output
            .map(|output| output.active_workspace_id)
            .unwrap_or(snapshot.session.active_workspace_id);
        signature.extend(
            snapshot
                .windows
                .iter()
                .filter(|window| {
                    crate::window_belongs_to_output(window, output, snapshot.outputs.len())
                        && window.workspace_id == workspace
                        && window.mapped
                        && !window.minimized
                })
                .map(|window| {
                    format!(
                        "{}:{}:{}:{}:{}",
                        window.id,
                        window.app_id.as_deref().unwrap_or_default(),
                        window.class.as_deref().unwrap_or_default(),
                        window.title,
                        window.focused,
                    )
                }),
        );
    } else {
        signature.push(
            chrono::Local::now()
                .format(crate::rail_clock_pattern(
                    crate::load_config().panel.clock_format,
                ))
                .to_string(),
        );
    }
    signature
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_buttons_dispatch_launch_actions_on_pointer_click() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let latest_snapshot = Rc::new(RefCell::new(None));
        let mut runtime = build_runtime(
            ShellRole::Dock,
            "DP-3",
            actions.clone(),
            latest_snapshot,
            Rc::new(RefCell::new(DockViewState::default())),
        );
        runtime.set_viewport(Size::new(560.0, 64.0));
        let frame = runtime.render_frame().expect("dock layout");

        let mut expected_buttons = [
            ("Browser", "@browser"),
            ("Terminal", "@terminal"),
            ("Files", "@files"),
            ("Application launcher", "@launcher"),
            ("Settings", "@settings"),
        ]
        .into_iter()
        .map(|(label, app)| (label.to_owned(), app.to_owned()))
        .collect::<Vec<_>>();
        if let Some(email) = crate::preferred_email_command() {
            expected_buttons.insert(3, ("Email".to_owned(), email));
        }

        for (label, expected_app) in expected_buttons {
            let button = frame
                .semantics
                .iter()
                .find(|node| node.role == vui_core::SemanticRole::Button && node.label == label)
                .unwrap_or_else(|| panic!("missing dock button {label}"));
            let position = Point::new(
                button.bounds.x + button.bounds.width / 2.0,
                button.bounds.y + button.bounds.height / 2.0,
            );
            runtime.handle_event(&Event::PointerDown {
                position,
                button: PointerButton::Primary,
                click_count: 1,
            });
            runtime.handle_event(&Event::PointerUp {
                position,
                button: PointerButton::Primary,
            });

            assert!(
                matches!(
                    actions.borrow_mut().pop(),
                    Some(DesktopAction::LaunchApp { app }) if app == expected_app
                ),
                "dock button {label} should dispatch {expected_app}"
            );
        }
    }

    #[test]
    fn dock_chooser_focuses_the_selected_window() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let window = WindowSnapshot {
            id: 42,
            title: "Project notes".into(),
            app_id: Some("org.example.Editor".into()),
            class: None,
            x11: false,
            workspace_id: 1,
            output_id: None,
            mapped: true,
            minimized: false,
            maximized: false,
            fullscreen: false,
            focused: false,
            x: None,
            y: None,
            width: None,
            height: None,
        };
        let view = Rc::new(RefCell::new(DockViewState {
            chooser: Some(vec![window]),
            page: 0,
        }));
        let mut runtime = build_runtime(
            ShellRole::Dock,
            "DP-3",
            actions.clone(),
            Rc::new(RefCell::new(None)),
            view,
        );
        runtime.set_viewport(Size::new(560.0, 64.0));
        let frame = runtime.render_frame().expect("chooser layout");
        let choice = frame
            .semantics
            .iter()
            .find(|node| node.role == vui_core::SemanticRole::Button && node.label == "Project")
            .expect("window choice");
        let position = Point::new(
            choice.bounds.x + choice.bounds.width / 2.0,
            choice.bounds.y + choice.bounds.height / 2.0,
        );
        runtime.handle_event(&Event::PointerDown {
            position,
            button: PointerButton::Primary,
            click_count: 1,
        });
        runtime.handle_event(&Event::PointerUp {
            position,
            button: PointerButton::Primary,
        });
        assert!(matches!(
            actions.borrow_mut().pop(),
            Some(DesktopAction::FocusWindow { window_id: 42 })
        ));
    }
}
