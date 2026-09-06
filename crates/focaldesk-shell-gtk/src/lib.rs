//! Reusable GTK4 shell toolkit for the FocalDesk panel and dock clients.
//!
//! This crate is a Wayland client. It consumes renderer-neutral theme tokens,
//! but intentionally has no dependency on `focaldesk-ui` or any compositor
//! renderer, shader, or icon atlas.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use focaldesk_config::{
    load_config, ClockFormat, DockPosition, DockSize, DockVisibility, FocalDeskConfig,
    PanelPosition, ShellStyle,
};
use focaldesk_ipc::{
    send_desktop_request, DesktopAction, DesktopSnapshot, IpcRequest, IpcResponse, OutputSnapshot,
    ShellPanel, WindowSnapshot,
};
use focaldesk_logging::{flog, init_default_logging, startup_banner};
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{theme_by_name, FlowTheme};
use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// Canonical shell geometry shared by the GTK panel and dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellMetrics {
    pub panel_height: i32,
    pub dock_width: i32,
    pub control_size: i32,
    pub control_gap: i32,
}

pub const DEFAULT_METRICS: ShellMetrics = ShellMetrics {
    panel_height: 64,
    dock_width: 76,
    // CSS content-box size; the 1px border on each side produces the
    // compositor's canonical 48px outer dock module.
    control_size: 46,
    control_gap: 8,
};

const SYSTEM_RAIL_WIDTH: i32 = 64;
const SYSTEM_RAIL_RESERVATION: i32 = 80;
const TASK_SHELF_WIDTH: i32 = 560;
const TASK_SHELF_HEIGHT: i32 = 64;
const TASK_SHELF_VISIBLE_MARGIN: i32 = 22;
const TASK_SHELF_REVEAL_STRIP: i32 = 4;
const TASK_SHELF_HIDE_DELAY: Duration = Duration::from_millis(300);
const TASK_SHELF_BUTTON_WIDTH: i32 = 48;
const TASK_SHELF_GROUP_GAP: i32 = 4;
// Outer padding/border and spacing, the separator, and the two utility buttons.
const TASK_SHELF_FIXED_WIDTH: i32 = 151;
// Focus follows the pointer across outputs, so the rail's active-output accent
// must not wait on the slower background-status refresh cadence.
const RAIL_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BACKGROUND_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const SHELL_CSS_BASE: &str = include_str!("shell.css");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRole {
    Panel,
    Dock,
}

impl ShellRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Panel => "focaldesk-system-rail",
            Self::Dock => "focaldesk-task-shelf",
        }
    }

    const fn application_id(self) -> &'static str {
        match self {
            Self::Panel => "dev.focaldesk.SystemRail",
            Self::Dock => "dev.focaldesk.TaskShelf",
        }
    }

    const fn layer_default_size(self) -> (i32, i32) {
        match self {
            Self::Panel => (SYSTEM_RAIL_WIDTH, 0),
            Self::Dock => (TASK_SHELF_WIDTH, TASK_SHELF_HEIGHT),
        }
    }
}

pub fn run(role: ShellRole) -> Result<()> {
    init_default_logging();
    startup_banner(
        role.namespace(),
        env!("CARGO_PKG_VERSION"),
        "gtk4-layer-shell",
    );

    let app = gtk::Application::builder()
        .application_id(role.application_id())
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| build_shell(app, role));
    app.run();
    Ok(())
}

fn build_shell(app: &gtk::Application, role: ShellRole) {
    install_theme();
    let config = Rc::new(load_config());
    let Some(display) = gdk::Display::default() else {
        flog(format!("{}: no GDK display", role.namespace()));
        app.quit();
        return;
    };

    rebuild_shell_windows(app, role, &display, &config);
    let app = app.clone();
    let display_for_changes = display.clone();
    let config_for_changes = config.clone();
    display.monitors().connect_items_changed(move |_, _, _, _| {
        rebuild_shell_windows(&app, role, &display_for_changes, &config_for_changes);
    });
}

fn rebuild_shell_windows(
    app: &gtk::Application,
    role: ShellRole,
    display: &gdk::Display,
    config: &FocalDeskConfig,
) {
    for window in app.windows() {
        window.close();
    }

    let monitors = display.monitors();
    let mut output_count = 0usize;
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else {
            continue;
        };
        let connector = monitor
            .connector()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("output-{}", index + 1));
        let window = match role {
            ShellRole::Dock => build_dock(app, config, connector),
            ShellRole::Panel => build_panel(app, index + 1, config, connector),
        };
        configure_layer_window(&window, role, &monitor, config);
        window.present();
        output_count += 1;
    }

    if output_count == 0 {
        let window = match role {
            ShellRole::Dock => build_dock(app, config, "output-1".into()),
            ShellRole::Panel => build_panel(app, 1, config, "output-1".into()),
        };
        configure_layer_window_without_monitor(&window, role, config);
        window.present();
        output_count = 1;
    }

    let _ = send_desktop_request(&IpcRequest::ShellReady {
        namespace: role.namespace().to_string(),
        output_count,
    });
    flog(format!(
        "{}: presented {output_count} GTK layer surface(s)",
        role.namespace()
    ));
}

fn configure_layer_window(
    window: &gtk::ApplicationWindow,
    role: ShellRole,
    monitor: &gdk::Monitor,
    config: &FocalDeskConfig,
) {
    configure_layer_window_without_monitor(window, role, config);
    window.set_monitor(monitor);
}

fn configure_layer_window_without_monitor(
    window: &gtk::ApplicationWindow,
    role: ShellRole,
    _config: &FocalDeskConfig,
) {
    window.init_layer_shell();
    window.set_namespace(role.namespace());
    window.set_layer(Layer::Top);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.add_css_class("floating");
    match role {
        ShellRole::Panel => {
            window.add_css_class("edge-right");
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Bottom, true);
            window.set_anchor(Edge::Right, true);
            let (width, height) = role.layer_default_size();
            window.set_default_size(width, height);
            window.set_margin(Edge::Top, 12);
            window.set_margin(Edge::Right, 12);
            window.set_margin(Edge::Bottom, 12);
            window.set_exclusive_zone(SYSTEM_RAIL_RESERVATION);
        }
        ShellRole::Dock => {
            window.add_css_class("edge-bottom");
            window.set_anchor(Edge::Bottom, true);
            let (width, height) = role.layer_default_size();
            window.set_default_size(width, height);
            window.set_margin(Edge::Bottom, TASK_SHELF_VISIBLE_MARGIN);
            window.set_exclusive_zone(0);
        }
    }
}

#[derive(Default)]
struct ShelfWidgets {
    running_signature: Vec<(u32, String, String, bool)>,
    pinned_count: usize,
    obscured: bool,
    pointer_inside: bool,
    overflow_open: bool,
    hidden: bool,
    hide_after: Option<Instant>,
}

#[derive(Clone)]
struct ShelfOverflowUi {
    revealer: gtk::Revealer,
    title: gtk::Label,
    list: gtk::Box,
    window: glib::WeakRef<gtk::ApplicationWindow>,
}

fn build_shelf_overflow_ui(
    window: &gtk::ApplicationWindow,
    shelf_state: &Rc<RefCell<ShelfWidgets>>,
) -> ShelfOverflowUi {
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    revealer.set_transition_duration(180);

    let surface = gtk::Box::new(gtk::Orientation::Vertical, 8);
    surface.add_css_class("shelf-overflow-surface");
    surface.add_css_class("shell-surface");
    revealer.set_child(Some(&surface));

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("shelf-overflow-header");
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("shelf-overflow-close");
    close.set_tooltip_text(Some("Close window list"));
    let close_revealer = revealer.clone();
    let close_window = window.downgrade();
    let close_state = shelf_state.clone();
    close.connect_clicked(move |_| {
        dismiss_shelf_overflow(&close_revealer, &close_window, &close_state);
        let mut state = close_state.borrow_mut();
        state.hide_after = Some(Instant::now() + TASK_SHELF_HIDE_DELAY);
    });
    header.append(&close);
    surface.append(&header);

    let list = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    list.add_css_class("shelf-overflow-list");
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&list));
    surface.append(&scroller);

    ShelfOverflowUi {
        revealer,
        title,
        list,
        window: window.downgrade(),
    }
}

fn build_dock(
    app: &gtk::Application,
    config: &FocalDeskConfig,
    connector: String,
) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("focal-shell-window");
    window.add_css_class("task-shelf-window");

    let widgets = Rc::new(RefCell::new(ShelfWidgets::default()));
    let overflow_ui = build_shelf_overflow_ui(&window, &widgets);

    let shelf = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    shelf.add_css_class("task-shelf");
    shelf.add_css_class("shell-surface");
    shelf.set_halign(gtk::Align::Center);
    shelf.set_valign(gtk::Align::Center);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("task-shelf-root");
    root.append(&overflow_ui.revealer);
    root.append(&shelf);
    window.set_child(Some(&root));

    let pinned = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    pinned.add_css_class("shelf-group");
    pinned.append(&shelf_launch_button(
        "web-browser-symbolic",
        "Browser",
        "@browser",
    ));
    pinned.append(&shelf_launch_button(
        "utilities-terminal-symbolic",
        "Terminal",
        "@terminal",
    ));
    pinned.append(&shelf_launch_button(
        "system-file-manager-symbolic",
        "Files",
        "@files",
    ));

    let email_command = Rc::new(RefCell::new(preferred_email_command()));
    let email_target = Rc::new(Cell::new(None::<u32>));
    let click_email_command = email_command.clone();
    let click_email_target = email_target.clone();
    let (email_button, _) =
        glass_icon_button("mail-unread-symbolic", "Email", "shelf-button", move || {
            if let Some(window_id) = click_email_target.get() {
                send_action(DesktopAction::FocusWindow { window_id });
            } else if let Some(app) = click_email_command.borrow().clone() {
                send_action(DesktopAction::LaunchApp { app });
            }
        });
    email_button.set_visible(email_command.borrow().is_some());
    pinned.append(&email_button);
    shelf.append(&pinned);

    let running = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    running.add_css_class("shelf-group");
    running.add_css_class("running-group");
    shelf.append(&running);
    shelf.append(&shelf_separator());

    let utilities = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    utilities.add_css_class("shelf-group");
    utilities.append(&shelf_launch_button(
        "view-app-grid-symbolic",
        "Application launcher",
        "@launcher",
    ));
    utilities.append(&shelf_launch_button(
        "preferences-system-symbolic",
        "Settings",
        "@settings",
    ));
    shelf.append(&utilities);

    let visibility = Rc::new(Cell::new(config.dock.visibility));
    let visibility_reload = visibility.clone();
    glib::timeout_add_seconds_local(1, move || {
        visibility_reload.set(load_config().dock.visibility);
        glib::ControlFlow::Continue
    });
    let email_reload_command = email_command.clone();
    let email_reload_button = email_button.clone();
    glib::timeout_add_seconds_local(1, move || {
        let command = preferred_email_command();
        email_reload_button.set_visible(command.is_some());
        *email_reload_command.borrow_mut() = command;
        glib::ControlFlow::Continue
    });

    let pointer = gtk::EventControllerMotion::new();
    let enter_widgets = widgets.clone();
    let enter_window = window.downgrade();
    pointer.connect_enter(move |_, _, _| {
        let Some(window) = enter_window.upgrade() else {
            return;
        };
        let mut state = enter_widgets.borrow_mut();
        state.pointer_inside = true;
        state.hide_after = None;
        set_shelf_hidden(&window, &mut state, false);
    });
    let leave_widgets = widgets.clone();
    pointer.connect_leave(move |_| {
        let mut state = leave_widgets.borrow_mut();
        state.pointer_inside = false;
        state.hide_after = Some(Instant::now() + TASK_SHELF_HIDE_DELAY);
    });
    window.add_controller(pointer);

    let snapshots = start_snapshot_poll(&window, BACKGROUND_SNAPSHOT_POLL_INTERVAL);
    let capacity_email_button = email_button.clone();
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let Some(_window) = weak_window.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if let Some(snapshot) = snapshots
            .try_lock()
            .ok()
            .and_then(|mut snapshot| snapshot.take())
        {
            let output = output_for_connector(&snapshot, &connector);
            let workspace = output
                .map(|output| output.active_workspace_id)
                .unwrap_or(snapshot.session.active_workspace_id);
            email_target.set(
                email_command
                    .borrow()
                    .as_deref()
                    .and_then(|command| existing_email_window(&snapshot, command)),
            );
            widgets.borrow_mut().obscured =
                output.is_some_and(|output| shelf_is_obscured(&snapshot, output, workspace));
            let signature = snapshot
                .windows
                .iter()
                .filter(|item| {
                    window_belongs_to_output(item, output, snapshot.outputs.len())
                        && item.workspace_id == workspace
                        && item.mapped
                        && !item.minimized
                })
                .map(|item| {
                    (
                        item.id,
                        item.app_id
                            .clone()
                            .or_else(|| item.class.clone())
                            .unwrap_or_default(),
                        item.title.clone(),
                        item.focused,
                    )
                })
                .collect::<Vec<_>>();
            let pinned_count = 3 + usize::from(capacity_email_button.is_visible());
            if widgets.borrow().running_signature != signature
                || widgets.borrow().pinned_count != pinned_count
            {
                rebuild_running_apps(
                    &running,
                    &snapshot,
                    output,
                    workspace,
                    pinned_count,
                    &widgets,
                    &overflow_ui,
                );
                let mut state = widgets.borrow_mut();
                state.running_signature = signature;
                state.pinned_count = pinned_count;
            }
        }
        update_shelf_visibility(&_window, &widgets, visibility.get(), Instant::now());
        glib::ControlFlow::Continue
    });

    window
}

fn update_shelf_visibility(
    window: &gtk::ApplicationWindow,
    widgets: &Rc<RefCell<ShelfWidgets>>,
    visibility: DockVisibility,
    now: Instant,
) {
    let mut state = widgets.borrow_mut();
    let should_hide = match visibility {
        DockVisibility::AlwaysVisible => false,
        DockVisibility::IntelligentDodge => state.obscured,
        DockVisibility::Autohide => true,
    } && !state.pointer_inside
        && !state.overflow_open
        && !window.is_active();

    if !should_hide {
        state.hide_after = None;
        set_shelf_hidden(window, &mut state, false);
        return;
    }

    let deadline = *state.hide_after.get_or_insert(now + TASK_SHELF_HIDE_DELAY);
    if now >= deadline {
        set_shelf_hidden(window, &mut state, true);
    }
}

fn set_shelf_hidden(window: &gtk::ApplicationWindow, state: &mut ShelfWidgets, hidden: bool) {
    if state.hidden == hidden {
        return;
    }
    let bottom_margin = if hidden {
        -(TASK_SHELF_HEIGHT - TASK_SHELF_REVEAL_STRIP)
    } else {
        TASK_SHELF_VISIBLE_MARGIN
    };
    window.set_margin(Edge::Bottom, bottom_margin);
    if hidden {
        window.add_css_class("shelf-hidden");
    } else {
        window.remove_css_class("shelf-hidden");
    }
    state.hidden = hidden;
}

fn shelf_is_obscured(snapshot: &DesktopSnapshot, output: &OutputSnapshot, workspace: u32) -> bool {
    snapshot.windows.iter().any(|window| {
        if !window.mapped
            || window.minimized
            || window.workspace_id != workspace
            || !window_belongs_to_output(window, Some(output), snapshot.outputs.len())
        {
            return false;
        }
        if window.fullscreen || window.maximized {
            return true;
        }
        window_overlaps_shelf(window, output)
    })
}

fn window_overlaps_shelf(window: &WindowSnapshot, output: &OutputSnapshot) -> bool {
    let shelf_width = TASK_SHELF_WIDTH.min(output.width.max(1));
    let shelf_left = output.x + (output.width - shelf_width) / 2;
    let shelf_top = output.y + output.height - TASK_SHELF_VISIBLE_MARGIN - TASK_SHELF_HEIGHT;
    let shelf_right = shelf_left + shelf_width;
    let shelf_bottom = shelf_top + TASK_SHELF_HEIGHT;
    let (Some(x), Some(y), Some(width), Some(height)) =
        (window.x, window.y, window.width, window.height)
    else {
        return false;
    };
    x < shelf_right
        && x.saturating_add(width) > shelf_left
        && y < shelf_bottom
        && y.saturating_add(height) > shelf_top
}

fn shelf_launch_button(icon: &str, tooltip: &str, app: &'static str) -> gtk::Button {
    glass_icon_button(icon, tooltip, "shelf-button", move || {
        send_action(DesktopAction::LaunchApp { app: app.into() });
    })
    .0
}

fn preferred_email_command() -> Option<String> {
    let settings = load_settings();
    if !settings.apps.pin_email_to_shelf {
        return None;
    }
    let configured = settings.apps.email.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    ["evolution", "thunderbird", "geary"]
        .into_iter()
        .find(|candidate| command_available(candidate))
        .map(str::to_string)
}

fn command_available(command: &str) -> bool {
    let Some(executable) = command.split_whitespace().next() else {
        return false;
    };
    if executable.contains('/') {
        return std::path::Path::new(executable).is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(executable).is_file())
    })
}

fn existing_email_window(snapshot: &DesktopSnapshot, command: &str) -> Option<u32> {
    snapshot
        .windows
        .iter()
        .filter(|window| window.mapped && !window.minimized)
        .find(|window| {
            let identity = window
                .app_id
                .as_deref()
                .or(window.class.as_deref())
                .unwrap_or_default()
                .to_ascii_lowercase();
            email_identity_matches(command, &identity)
        })
        .map(|window| window.id)
}

fn email_identity_matches(command: &str, identity: &str) -> bool {
    let command = command.to_ascii_lowercase();
    let identity = identity.to_ascii_lowercase();
    let executable = command
        .split_whitespace()
        .next()
        .and_then(|part| std::path::Path::new(part).file_name())
        .and_then(|part| part.to_str())
        .unwrap_or_default();
    (!executable.is_empty() && identity.contains(executable))
        || ["evolution", "thunderbird", "geary", "mailspring"]
            .iter()
            .any(|token| command.contains(token) && identity.contains(token))
}

fn shelf_separator() -> gtk::Separator {
    let separator = gtk::Separator::new(gtk::Orientation::Vertical);
    separator.add_css_class("shelf-separator");
    separator
}

fn shelf_group_width(buttons: usize) -> i32 {
    if buttons == 0 {
        return 0;
    }
    buttons as i32 * TASK_SHELF_BUTTON_WIDTH
        + buttons.saturating_sub(1) as i32 * TASK_SHELF_GROUP_GAP
}

fn running_app_capacity(pinned_count: usize, reserve_overflow_button: bool) -> usize {
    let mut available = TASK_SHELF_WIDTH - TASK_SHELF_FIXED_WIDTH - shelf_group_width(pinned_count);
    if reserve_overflow_button {
        available -= TASK_SHELF_BUTTON_WIDTH + TASK_SHELF_GROUP_GAP;
    }
    ((available + TASK_SHELF_GROUP_GAP).max(0) / (TASK_SHELF_BUTTON_WIDTH + TASK_SHELF_GROUP_GAP))
        as usize
}

fn rebuild_running_apps(
    container: &gtk::Box,
    snapshot: &DesktopSnapshot,
    output: Option<&OutputSnapshot>,
    workspace: u32,
    pinned_count: usize,
    shelf_state: &Rc<RefCell<ShelfWidgets>>,
    overflow_ui: &ShelfOverflowUi,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let windows = snapshot
        .windows
        .iter()
        .filter(|item| {
            window_belongs_to_output(item, output, snapshot.outputs.len())
                && item.workspace_id == workspace
                && item.mapped
                && !item.minimized
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        container.set_visible(false);
        return;
    }
    container.set_visible(true);
    let mut groups: Vec<(String, Vec<&focaldesk_ipc::WindowSnapshot>)> = Vec::new();
    for item in windows {
        let identity = item
            .app_id
            .as_deref()
            .or(item.class.as_deref())
            .unwrap_or(&item.title)
            .to_ascii_lowercase();
        if let Some((_, windows)) = groups.iter_mut().find(|(key, _)| *key == identity) {
            windows.push(item);
        } else {
            groups.push((identity, vec![item]));
        }
    }

    let total_windows = groups
        .iter()
        .map(|(_, windows)| windows.len())
        .sum::<usize>();
    let needs_overflow_button = groups.len() > running_app_capacity(pinned_count, false);
    let visible_capacity = if needs_overflow_button {
        running_app_capacity(pinned_count, true)
    } else {
        groups.len()
    };
    let mut visible_indices = (0..visible_capacity.min(groups.len())).collect::<Vec<_>>();
    if let Some(focused_index) = groups
        .iter()
        .position(|(_, windows)| windows.iter().any(|window| window.focused))
    {
        if !visible_indices.contains(&focused_index) {
            if let Some(last) = visible_indices.last_mut() {
                *last = focused_index;
            }
        }
    }
    for index in visible_indices {
        let (identity, windows) = &groups[index];
        let icon = app_icon_name(Some(&identity));
        let target = windows
            .iter()
            .find(|window| window.focused)
            .copied()
            .unwrap_or(windows[0]);
        let tooltip = if windows.len() == 1 {
            target.title.clone()
        } else {
            format!("{} windows — click to choose", windows.len())
        };
        let (button, _) = if windows.len() > 1 {
            let entries = windows
                .iter()
                .map(|window| ShelfOverflowEntry {
                    id: window.id,
                    identity: identity.clone(),
                    title: window.title.clone(),
                    focused: window.focused,
                })
                .collect::<Vec<_>>();
            let group_ui = overflow_ui.clone();
            let group_state = shelf_state.clone();
            glass_icon_button(icon, &tooltip, "shelf-button", move || {
                eprintln!(
                    "focaldesk-task-shelf: opening chooser for one application / {} windows",
                    entries.len()
                );
                present_shelf_overflow(&group_ui, 1, &entries, &group_state);
            })
        } else {
            let id = target.id;
            glass_icon_button(icon, &tooltip, "shelf-button", move || {
                send_action(DesktopAction::FocusWindow { window_id: id });
            })
        };
        button.add_css_class("running-app");
        set_button_active(&button, windows.iter().any(|window| window.focused));
        if windows.len() > 1 {
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&button));
            let marks = gtk::Label::new(Some("≡"));
            marks.add_css_class("window-stack-marks");
            marks.set_can_target(false);
            marks.set_halign(gtk::Align::End);
            marks.set_valign(gtk::Align::Start);
            overlay.add_overlay(&marks);
            container.append(&overlay);
        } else {
            container.append(&button);
        }
    }

    if needs_overflow_button {
        let entries = groups
            .iter()
            .flat_map(|(identity, windows)| {
                windows.iter().map(|window| ShelfOverflowEntry {
                    id: window.id,
                    identity: identity.clone(),
                    title: window.title.clone(),
                    focused: window.focused,
                })
            })
            .collect::<Vec<_>>();
        let app_count = groups.len();
        let more = gtk::Button::with_label(&total_windows.to_string());
        more.add_css_class("shelf-button");
        more.add_css_class("shelf-overflow");
        more.set_tooltip_text(Some(&format!(
            "Open all {total_windows} windows across {app_count} applications"
        )));
        let overflow_state = shelf_state.clone();
        let overflow_ui = overflow_ui.clone();
        more.connect_clicked(move |_| {
            eprintln!(
                "focaldesk-task-shelf: opening chooser for {} applications / {} windows",
                app_count,
                entries.len()
            );
            present_shelf_overflow(&overflow_ui, app_count, &entries, &overflow_state);
        });
        container.append(&more);
    }
}

#[derive(Clone)]
struct ShelfOverflowEntry {
    id: u32,
    identity: String,
    title: String,
    focused: bool,
}

fn present_shelf_overflow(
    ui: &ShelfOverflowUi,
    app_count: usize,
    entries: &[ShelfOverflowEntry],
    shelf_state: &Rc<RefCell<ShelfWidgets>>,
) {
    ui.title.set_label(&format!(
        "{app_count} applications · {} windows",
        entries.len()
    ));
    while let Some(child) = ui.list.first_child() {
        ui.list.remove(&child);
    }
    for (index, entry) in entries.iter().enumerate() {
        let icon_name = app_icon_name(Some(&entry.identity));
        let row = gtk::Button::new();
        row.add_css_class("shelf-overflow-row");
        if entry.focused {
            row.add_css_class("active");
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let image = shell_icon_image(icon_name);
        image.set_pixel_size(48);
        set_shell_icon_at_size(&image, icon_name, 48);
        image.set_halign(gtk::Align::Center);
        content.append(&image);
        let title = shelf_window_label(entry, index);
        let label = gtk::Label::new(Some(&title));
        label.set_xalign(0.5);
        label.set_justify(gtk::Justification::Center);
        label.set_max_width_chars(18);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&label);
        row.set_child(Some(&content));
        row.set_tooltip_text(Some(&title));
        let id = entry.id;
        let row_ui = ui.clone();
        let row_state = shelf_state.clone();
        row.connect_clicked(move |_| {
            send_action(DesktopAction::FocusWindow { window_id: id });
            dismiss_shelf_overflow(&row_ui.revealer, &row_ui.window, &row_state);
            let mut state = row_state.borrow_mut();
            state.hide_after = Some(Instant::now() + TASK_SHELF_HIDE_DELAY);
        });
        ui.list.append(&row);
    }
    if let Some(window) = ui.window.upgrade() {
        window.set_default_size(760, 300);
        window.present();
    }
    ui.revealer.set_reveal_child(true);
    let mut state = shelf_state.borrow_mut();
    state.overflow_open = true;
    state.hide_after = None;
}

fn dismiss_shelf_overflow(
    revealer: &gtk::Revealer,
    window: &glib::WeakRef<gtk::ApplicationWindow>,
    shelf_state: &Rc<RefCell<ShelfWidgets>>,
) {
    revealer.set_reveal_child(false);
    shelf_state.borrow_mut().overflow_open = false;

    let revealer = revealer.downgrade();
    let window = window.clone();
    let shelf_state = shelf_state.clone();
    glib::timeout_add_local_once(Duration::from_millis(190), move || {
        if shelf_state.borrow().overflow_open {
            return;
        }
        if revealer
            .upgrade()
            .is_some_and(|revealer| revealer.reveals_child())
        {
            return;
        }
        if let Some(window) = window.upgrade() {
            window.set_default_size(TASK_SHELF_WIDTH, TASK_SHELF_HEIGHT);
        }
    });
}

fn shelf_window_label(entry: &ShelfOverflowEntry, index: usize) -> String {
    let title = entry.title.trim();
    if !title.is_empty()
        && !title.eq_ignore_ascii_case("untitled")
        && !title.eq_ignore_ascii_case("wayland app")
    {
        return title.to_string();
    }

    let identity = entry.identity.trim();
    if !identity.is_empty()
        && !identity.eq_ignore_ascii_case("untitled")
        && !identity.eq_ignore_ascii_case("wayland app")
    {
        return format!("{} · Window {}", humanize_app_identity(identity), index + 1);
    }
    format!("Window {}", index + 1)
}

fn humanize_app_identity(identity: &str) -> String {
    let leaf = identity.rsplit('.').next().unwrap_or(identity);
    let words = leaf.replace(['-', '_'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Application".into(),
    }
}

fn window_belongs_to_output(
    window: &WindowSnapshot,
    output: Option<&OutputSnapshot>,
    output_count: usize,
) -> bool {
    let Some(output) = output else {
        return output_count == 1 || window.output_id.is_none();
    };
    if let Some(output_id) = window.output_id {
        return output_id == output.id;
    }

    // XWayland and newly mapped windows can briefly lack an explicit output.
    // Geometry is the strongest fallback; focus keeps geometry-less windows on
    // exactly one shelf instead of duplicating them across every monitor.
    if let (Some(x), Some(y), Some(width), Some(height)) =
        (window.x, window.y, window.width, window.height)
    {
        let center_x = x.saturating_add(width / 2);
        let center_y = y.saturating_add(height / 2);
        return center_x >= output.x
            && center_x < output.x.saturating_add(output.width)
            && center_y >= output.y
            && center_y < output.y.saturating_add(output.height);
    }

    output_count == 1 || (window.focused && output.focused)
}

fn app_icon_name(identity: Option<&str>) -> &'static str {
    let identity = identity.unwrap_or_default().to_ascii_lowercase();
    if identity.contains("terminal") || identity.contains("foot") {
        "utilities-terminal-symbolic"
    } else if identity.contains("file") || identity.contains("nautilus") {
        "system-file-manager-symbolic"
    } else if identity.contains("mail") || identity.contains("evolution") {
        "mail-unread-symbolic"
    } else if identity.contains("browser")
        || identity.contains("chrome")
        || identity.contains("firefox")
    {
        "web-browser-symbolic"
    } else {
        "application-x-executable-symbolic"
    }
}

#[cfg(any())]
struct PanelWidgets {
    title: gtk::Label,
    network_button: gtk::Button,
    network_image: gtk::Image,
    microphone_button: gtk::Button,
    microphone_image: gtk::Image,
    notifications_button: gtk::Button,
    notifications_image: gtk::Image,
    updates_button: gtk::Button,
    updates_image: gtk::Image,
    dnd_button: gtk::Button,
    dnd_image: gtk::Image,
    camera_button: gtk::Button,
    camera_image: gtk::Image,
    display_button: gtk::Button,
    clock: gtk::Label,
}

#[cfg(any())]
fn build_panel(
    app: &gtk::Application,
    output_number: u32,
    config: &FocalDeskConfig,
    connector: String,
) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("focal-shell-window");
    window.add_css_class("focal-panel-window");

    let panel = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    panel.add_css_class("focal-panel");
    panel.set_height_request(PANEL_HEIGHT);
    window.set_child(Some(&panel));

    let corner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    corner.add_css_class("panel-corner");
    // The compositor's top-bar inner frame begins four pixels after the dock.
    corner.set_width_request(DOCK_WIDTH + 4);
    panel.append(&corner);

    let panel_inner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    panel_inner.add_css_class("panel-inner");
    panel_inner.set_hexpand(true);
    panel_inner.set_margin_top(3);
    panel_inner.set_margin_bottom(3);
    panel_inner.set_margin_end(4);
    panel.append(&panel_inner);

    let (launcher, _) = glass_icon_button(
        "focaldesk-ai-console",
        "Launch FocalDesk AI Console",
        "panel-launcher",
        || {
            send_action(DesktopAction::LaunchApp {
                app: "focaldesk-ai-console".into(),
            });
        },
    );
    launcher.add_css_class("panel-well");
    launcher.set_margin_start(6);
    launcher.set_margin_end(12);
    panel_inner.append(&launcher);

    let title = gtk::Label::new(Some(&format!("FOCALDESK · OUT {output_number} · WS 1")));
    title.add_css_class("panel-title");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Fill);
    title.set_valign(gtk::Align::Center);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let title_region = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    title_region.add_css_class("panel-well");
    title_region.add_css_class("panel-title-region");
    title_region.set_hexpand(true);
    title_region.set_halign(gtk::Align::Fill);
    title_region.set_valign(gtk::Align::Center);
    title_region.set_margin_end(10);
    title_region.append(&title);
    panel_inner.append(&title_region);

    let status_cluster = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status_cluster.add_css_class("panel-status-cluster");
    status_cluster.set_valign(gtk::Align::Center);

    let network_connector = connector.clone();
    let (network_button, network_image) = glass_icon_button(
        "network-wireless-offline-symbolic",
        "Network",
        "panel-status-button",
        move || {
            open_shell_panel(&network_connector, ShellPanel::Network);
        },
    );
    status_cluster.append(&network_button);
    let bluetooth_connector = connector.clone();
    status_cluster.append(
        &glass_icon_button(
            "bluetooth-active-symbolic",
            "Bluetooth",
            "panel-status-button",
            move || {
                open_shell_panel(&bluetooth_connector, ShellPanel::Bluetooth);
            },
        )
        .0,
    );
    let audio_connector = connector.clone();
    status_cluster.append(
        &glass_icon_button(
            "audio-volume-high-symbolic",
            "Output volume",
            "panel-status-button",
            move || {
                open_shell_panel(&audio_connector, ShellPanel::Audio);
            },
        )
        .0,
    );
    let microphone_connector = connector.clone();
    let (microphone_button, microphone_image) = glass_icon_button(
        "microphone-sensitivity-muted-symbolic",
        "Voice input",
        "panel-status-button",
        move || {
            open_shell_panel(&microphone_connector, ShellPanel::Audio);
        },
    );
    status_cluster.append(&microphone_button);
    let notifications_connector = connector.clone();
    let (notifications_button, notifications_image) = glass_icon_button(
        "notification-symbolic",
        "Notifications",
        "panel-status-button",
        move || open_shell_panel(&notifications_connector, ShellPanel::NotificationHistory),
    );
    status_cluster.append(&notifications_button);
    let updates_connector = connector.clone();
    let (updates_button, updates_image) = glass_icon_button(
        "software-update-available-symbolic",
        "System updates",
        "panel-status-button",
        move || open_shell_panel(&updates_connector, ShellPanel::Updates),
    );
    status_cluster.append(&updates_button);
    let (dnd_button, dnd_image) = glass_icon_button(
        "notifications-disabled-symbolic",
        "Do Not Disturb",
        "panel-status-button",
        || send_action(DesktopAction::ToggleDoNotDisturb),
    );
    status_cluster.append(&dnd_button);
    let camera_connector = connector.clone();
    let (camera_button, camera_image) = glass_icon_button(
        "camera-disabled-symbolic",
        "Camera",
        "panel-status-button",
        move || {
            open_shell_panel(&camera_connector, ShellPanel::Settings);
        },
    );
    status_cluster.append(&camera_button);
    let display_connector = connector.clone();
    let (display_button, _) = glass_icon_button(
        "video-display-symbolic",
        "Displays",
        "panel-status-button",
        move || {
            open_shell_panel(&display_connector, ShellPanel::Display);
        },
    );
    status_cluster.append(&display_button);
    let power_connector = connector.clone();
    status_cluster.append(
        &glass_icon_button(
            "system-shutdown-symbolic",
            "Power",
            "panel-status-button",
            move || {
                open_shell_panel(&power_connector, ShellPanel::Power);
            },
        )
        .0,
    );
    panel_inner.append(&status_cluster);

    let clock = gtk::Label::new(None);
    clock.add_css_class("panel-clock");
    let clock_button = gtk::Button::new();
    clock_button.add_css_class("panel-well");
    clock_button.add_css_class("panel-clock-button");
    clock_button.set_valign(gtk::Align::Center);
    clock_button.set_margin_start(8);
    clock_button.set_margin_end(10);
    clock_button.set_tooltip_text(Some("Calendar and clock"));
    clock_button.set_child(Some(&clock));
    let clock_connector = connector.clone();
    clock_button.connect_clicked(move |_| open_shell_panel(&clock_connector, ShellPanel::Calendar));
    panel_inner.append(&clock_button);

    let widgets = PanelWidgets {
        title,
        network_button,
        network_image,
        microphone_button,
        microphone_image,
        notifications_button,
        notifications_image,
        updates_button,
        updates_image,
        dnd_button,
        dnd_image,
        camera_button,
        camera_image,
        display_button,
        clock,
    };
    let clock_format = config.panel.clock_format;
    update_clock(&widgets.clock, clock_format);
    let clock = widgets.clock.clone();
    glib::timeout_add_seconds_local(1, move || {
        update_clock(&clock, clock_format);
        glib::ControlFlow::Continue
    });
    let snapshots = start_snapshot_poll(&window, BACKGROUND_SNAPSHOT_POLL_INTERVAL);
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let Some(_window) = weak_window.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let snapshot = snapshots
            .try_lock()
            .ok()
            .and_then(|mut snapshot| snapshot.take());
        if let Some(snapshot) = snapshot {
            update_panel(&widgets, &snapshot, &connector, output_number);
        }
        glib::ControlFlow::Continue
    });

    window
}

#[cfg(any())]
fn update_panel(
    widgets: &PanelWidgets,
    snapshot: &DesktopSnapshot,
    connector: &str,
    output_number: u32,
) {
    let output = output_for_connector(snapshot, connector);
    let active_workspace = output
        .map(|output| output.active_workspace_id)
        .unwrap_or(snapshot.session.active_workspace_id)
        .max(1);
    // Match the compositor-owned identity exactly. The connector still selects
    // this output's state; the visible identity uses the stable shell numbering.
    widgets.title.set_text(&format!(
        "FOCALDESK · OUT {output_number} · WS {active_workspace}"
    ));
    set_shell_icon(
        &widgets.network_image,
        if snapshot.shell.network_carrier {
            "network-wireless-signal-excellent-symbolic"
        } else {
            "network-wireless-offline-symbolic"
        },
    );
    set_button_active(&widgets.network_button, snapshot.shell.network_carrier);
    set_shell_icon(
        &widgets.microphone_image,
        if snapshot.shell.microphone_active {
            "audio-input-microphone-symbolic"
        } else {
            "microphone-sensitivity-muted-symbolic"
        },
    );
    set_button_active(&widgets.microphone_button, snapshot.shell.microphone_active);
    widgets
        .microphone_button
        .set_tooltip_text(Some(if snapshot.shell.microphone_active {
            "Voice input: listening"
        } else if snapshot.shell.microphone_detected {
            "Voice input: not listening"
        } else {
            "No microphone detected"
        }));
    set_shell_icon(
        &widgets.notifications_image,
        if snapshot.shell.notification_unread_count > 0 {
            "notification-new-symbolic"
        } else {
            "notification-symbolic"
        },
    );
    set_button_active(
        &widgets.notifications_button,
        snapshot.shell.notification_unread_count > 0,
    );
    set_shell_icon(
        &widgets.updates_image,
        if snapshot.shell.update_busy {
            "emblem-synchronizing-symbolic"
        } else if snapshot.shell.update_available_count > 0 {
            "software-update-available-symbolic"
        } else {
            "software-update-available-symbolic"
        },
    );
    set_button_active(
        &widgets.updates_button,
        snapshot.shell.update_available_count > 0 || snapshot.shell.update_busy,
    );
    let updates_tooltip = if snapshot.shell.update_busy {
        "Installing or checking system updates…".to_string()
    } else if snapshot.shell.update_available_count == 0 {
        "System updates: up to date".to_string()
    } else if snapshot.shell.update_available_count == 1 {
        "1 system update available".to_string()
    } else {
        format!(
            "{} system updates available",
            snapshot.shell.update_available_count
        )
    };
    widgets
        .updates_button
        .set_tooltip_text(Some(&updates_tooltip));
    set_shell_icon(&widgets.dnd_image, "notifications-disabled-symbolic");
    set_button_active(&widgets.dnd_button, snapshot.shell.do_not_disturb);
    set_shell_icon(
        &widgets.camera_image,
        if snapshot.shell.camera_active {
            "camera-web-symbolic"
        } else {
            "camera-disabled-symbolic"
        },
    );
    set_button_active(&widgets.camera_button, snapshot.shell.camera_active);
    widgets
        .camera_button
        .set_tooltip_text(Some(if snapshot.shell.camera_active {
            "Camera in use"
        } else if snapshot.shell.camera_detected {
            "Camera detected — not in use"
        } else {
            "No camera detected"
        }));
    if let Some(output) = output {
        set_button_active(&widgets.display_button, output.hdr_active);
        widgets
            .display_button
            .set_tooltip_text(Some(if output.hdr_active {
                "Displays: HDR active"
            } else if output.hdr_supported {
                "Displays: HDR available"
            } else {
                "Displays"
            }));
    }
}

#[derive(Default)]
struct RailWorkspaceWidgets {
    workspace_count: usize,
    buttons: Vec<gtk::Button>,
}

struct SystemRailWidgets {
    focus_notch: gtk::Box,
    network_button: gtk::Button,
    network_image: gtk::Image,
    microphone_button: gtk::Button,
    camera_button: gtk::Button,
    display_button: gtk::Button,
    display_badge: gtk::Label,
    battery_button: gtk::Button,
    battery_image: gtk::Image,
    battery: gtk::Label,
    notifications_button: gtk::Button,
    clock: gtk::Label,
    workspaces: Rc<RefCell<RailWorkspaceWidgets>>,
    workspace_box: gtk::Box,
    add_workspace_button: gtk::Button,
    remove_workspace_button: gtk::Button,
}

fn build_panel(
    app: &gtk::Application,
    _output_number: u32,
    config: &FocalDeskConfig,
    connector: String,
) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("focal-shell-window");
    window.add_css_class("system-rail-window");

    let rail = gtk::Box::new(gtk::Orientation::Vertical, 8);
    rail.add_css_class("system-rail");
    rail.add_css_class("shell-surface");
    let rail_overlay = gtk::Overlay::new();
    rail_overlay.set_child(Some(&rail));
    let focus_notch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    focus_notch.add_css_class("rail-output-focus-notch");
    focus_notch.set_width_request(24);
    focus_notch.set_height_request(3);
    focus_notch.set_halign(gtk::Align::Center);
    focus_notch.set_valign(gtk::Align::Start);
    focus_notch.set_can_target(false);
    focus_notch.set_visible(false);
    rail_overlay.add_overlay(&focus_notch);
    window.set_child(Some(&rail_overlay));

    let top = gtk::Box::new(gtk::Orientation::Vertical, 4);
    top.add_css_class("rail-group");
    let network_connector = connector.clone();
    let (network_button, network_image) = glass_icon_button(
        "network-wireless-offline-symbolic",
        "Network",
        "rail-button",
        move || open_shell_panel(&network_connector, ShellPanel::Network),
    );
    top.append(&network_button);
    let audio_connector = connector.clone();
    top.append(
        &glass_icon_button(
            "audio-volume-high-symbolic",
            "Audio",
            "rail-button",
            move || open_shell_panel(&audio_connector, ShellPanel::Audio),
        )
        .0,
    );
    let bluetooth_connector = connector.clone();
    top.append(
        &glass_icon_button(
            "bluetooth-active-symbolic",
            "Bluetooth",
            "rail-button",
            move || open_shell_panel(&bluetooth_connector, ShellPanel::Bluetooth),
        )
        .0,
    );
    let microphone_connector = connector.clone();
    let (microphone_button, _) = glass_icon_button(
        "audio-input-microphone-symbolic",
        "Microphone in use",
        "rail-button",
        move || open_shell_panel(&microphone_connector, ShellPanel::Audio),
    );
    microphone_button.add_css_class("rail-privacy-indicator");
    microphone_button.set_visible(false);
    top.append(&microphone_button);
    let camera_connector = connector.clone();
    let (camera_button, _) = glass_icon_button(
        "camera-web-symbolic",
        "Camera in use",
        "rail-button",
        move || open_shell_panel(&camera_connector, ShellPanel::Settings),
    );
    camera_button.add_css_class("rail-privacy-indicator");
    camera_button.set_visible(false);
    top.append(&camera_button);
    let display_button = gtk::Button::new();
    display_button.add_css_class("rail-button");
    display_button.set_tooltip_text(Some("Display settings"));
    let display_overlay = gtk::Overlay::new();
    let display_image = shell_icon_image("video-display-symbolic");
    display_image.set_pixel_size(26);
    display_overlay.set_child(Some(&display_image));
    let display_badge = gtk::Label::new(None);
    display_badge.add_css_class("rail-display-badge");
    display_badge.set_halign(gtk::Align::End);
    display_badge.set_valign(gtk::Align::Start);
    display_badge.set_visible(false);
    display_overlay.add_overlay(&display_badge);
    display_button.set_child(Some(&display_overlay));
    let display_connector = connector.clone();
    display_button
        .connect_clicked(move |_| open_shell_panel(&display_connector, ShellPanel::Display));
    top.append(&display_button);
    let battery = gtk::Label::new(None);
    battery.add_css_class("rail-battery-label");
    let battery_button = gtk::Button::new();
    battery_button.add_css_class("rail-button");
    battery_button.add_css_class("rail-battery-control");
    battery_button.set_tooltip_text(Some("Battery and power"));
    let battery_overlay = gtk::Overlay::new();
    let battery_image = shell_icon_image("ac-adapter-symbolic");
    battery_image.set_pixel_size(26);
    set_shell_icon_at_size(&battery_image, "ac-adapter-symbolic", 26);
    battery_overlay.set_child(Some(&battery_image));
    battery.set_halign(gtk::Align::End);
    battery.set_valign(gtk::Align::End);
    battery_overlay.add_overlay(&battery);
    battery_button.set_child(Some(&battery_overlay));
    let battery_connector = connector.clone();
    battery_button
        .connect_clicked(move |_| open_shell_panel(&battery_connector, ShellPanel::Power));
    top.append(&battery_button);
    rail.append(&top);

    let workspace_cluster = gtk::Box::new(gtk::Orientation::Vertical, 4);
    workspace_cluster.add_css_class("rail-workspace-cluster");
    workspace_cluster.set_vexpand(true);
    workspace_cluster.set_valign(gtk::Align::Center);
    let workspace_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    workspace_box.add_css_class("rail-workspaces");
    workspace_cluster.append(&workspace_box);
    let workspaces = Rc::new(RefCell::new(RailWorkspaceWidgets::default()));
    rebuild_rail_workspaces(&workspace_box, &workspaces, 1, &connector);

    let add_workspace_button = gtk::Button::with_label("+");
    add_workspace_button.add_css_class("rail-button");
    add_workspace_button.add_css_class("rail-workspace-control");
    add_workspace_button.set_tooltip_text(Some("Add workspace"));
    let add_connector = connector.clone();
    add_workspace_button.connect_clicked(move |_| {
        send_action(DesktopAction::CreateWorkspaceOnOutput {
            connector: add_connector.clone(),
        });
    });
    workspace_cluster.append(&add_workspace_button);

    let remove_workspace_button = gtk::Button::with_label("−");
    remove_workspace_button.add_css_class("rail-button");
    remove_workspace_button.add_css_class("rail-workspace-control");
    remove_workspace_button.set_tooltip_text(Some("Delete current workspace"));
    remove_workspace_button.set_visible(false);
    let remove_connector = connector.clone();
    remove_workspace_button.connect_clicked(move |_| {
        send_action(DesktopAction::DeleteWorkspaceOnOutput {
            connector: remove_connector.clone(),
        });
    });
    workspace_cluster.append(&remove_workspace_button);
    rail.append(&workspace_cluster);

    let bottom = gtk::Box::new(gtk::Orientation::Vertical, 4);
    bottom.add_css_class("rail-group");
    let clock = gtk::Label::new(None);
    clock.add_css_class("rail-clock");
    let clock_button = gtk::Button::new();
    clock_button.add_css_class("rail-button");
    clock_button.add_css_class("rail-clock-control");
    clock_button.set_tooltip_text(Some("Calendar and clock"));
    clock_button.set_child(Some(&clock));
    let clock_connector = connector.clone();
    clock_button.connect_clicked(move |_| open_shell_panel(&clock_connector, ShellPanel::Calendar));
    bottom.append(&clock_button);
    let notifications_connector = connector.clone();
    let (notifications_button, _) = glass_icon_button(
        "notification-symbolic",
        "Notification center",
        "rail-button",
        move || open_shell_panel(&notifications_connector, ShellPanel::NotificationHistory),
    );
    bottom.append(&notifications_button);
    let power_connector = connector.clone();
    bottom.append(
        &glass_icon_button(
            "system-shutdown-symbolic",
            "Power menu",
            "rail-button",
            move || open_shell_panel(&power_connector, ShellPanel::Power),
        )
        .0,
    );
    rail.append(&bottom);

    let widgets = SystemRailWidgets {
        focus_notch,
        network_button,
        network_image,
        microphone_button,
        camera_button,
        display_button,
        display_badge,
        battery_button,
        battery_image,
        battery,
        notifications_button,
        clock,
        workspaces,
        workspace_box,
        add_workspace_button,
        remove_workspace_button,
    };
    let clock_format = config.panel.clock_format;
    update_clock(&widgets.clock, clock_format);
    let clock = widgets.clock.clone();
    glib::timeout_add_seconds_local(1, move || {
        update_clock(&clock, clock_format);
        glib::ControlFlow::Continue
    });

    let snapshots = start_snapshot_poll(&window, RAIL_SNAPSHOT_POLL_INTERVAL);
    let weak_window = window.downgrade();
    glib::timeout_add_local(RAIL_SNAPSHOT_POLL_INTERVAL, move || {
        let Some(_window) = weak_window.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if let Some(snapshot) = snapshots
            .try_lock()
            .ok()
            .and_then(|mut snapshot| snapshot.take())
        {
            update_system_rail(&widgets, &snapshot, &connector);
        }
        glib::ControlFlow::Continue
    });
    window
}

fn rebuild_rail_workspaces(
    container: &gtk::Box,
    widgets: &Rc<RefCell<RailWorkspaceWidgets>>,
    count: usize,
    connector: &str,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let mut buttons = Vec::new();
    for workspace in 1..=count.min(9) {
        let button = gtk::Button::with_label(&workspace.to_string());
        button.add_css_class("rail-button");
        button.add_css_class("rail-workspace-button");
        button.set_tooltip_text(Some(&format!("Workspace {workspace}")));
        let connector = connector.to_string();
        button.connect_clicked(move |_| {
            send_action(DesktopAction::FocusWorkspaceOnOutput {
                connector: connector.clone(),
                workspace: workspace as u32,
            });
        });
        container.append(&button);
        buttons.push(button);
    }
    *widgets.borrow_mut() = RailWorkspaceWidgets {
        workspace_count: count,
        buttons,
    };
}

fn update_system_rail(widgets: &SystemRailWidgets, snapshot: &DesktopSnapshot, connector: &str) {
    let output = output_for_connector(snapshot, connector);
    widgets
        .focus_notch
        .set_visible(output.is_some_and(|output| output.focused));
    let active_workspace = output
        .map(|output| output.active_workspace_id)
        .unwrap_or(snapshot.session.active_workspace_id)
        .max(1);
    let count = snapshot.shell.workspace_count.max(1);
    let maximum = snapshot.shell.max_workspace_slots.max(1);
    if widgets.workspaces.borrow().workspace_count != count {
        rebuild_rail_workspaces(
            &widgets.workspace_box,
            &widgets.workspaces,
            count,
            connector,
        );
    }
    for (index, button) in widgets.workspaces.borrow().buttons.iter().enumerate() {
        set_button_active(button, active_workspace == index as u32 + 1);
    }
    widgets.add_workspace_button.set_sensitive(count < maximum);
    widgets.remove_workspace_button.set_visible(count > 1);
    if let Some(output) = output {
        let status = rail_display_status(output);
        widgets
            .display_button
            .set_tooltip_text(Some(&status.tooltip));
        widgets.display_badge.set_label(status.badge.unwrap_or(""));
        widgets.display_badge.set_visible(status.badge.is_some());
        if status.warning {
            widgets.display_badge.add_css_class("warning");
        } else {
            widgets.display_badge.remove_css_class("warning");
        }
    } else {
        widgets
            .display_button
            .set_tooltip_text(Some("Display settings"));
        widgets.display_badge.set_visible(false);
        widgets.display_badge.remove_css_class("warning");
    }
    set_shell_icon(
        &widgets.network_image,
        if snapshot.shell.network_carrier {
            "network-wireless-signal-excellent-symbolic"
        } else {
            "network-wireless-offline-symbolic"
        },
    );
    set_button_active(&widgets.network_button, snapshot.shell.network_carrier);
    widgets
        .microphone_button
        .set_visible(snapshot.shell.microphone_active);
    set_button_active(&widgets.microphone_button, snapshot.shell.microphone_active);
    widgets
        .camera_button
        .set_visible(snapshot.shell.camera_active);
    set_button_active(&widgets.camera_button, snapshot.shell.camera_active);
    let externally_powered =
        snapshot.shell.line_power_online == Some(true) || snapshot.shell.battery_percent.is_none();
    set_shell_icon(
        &widgets.battery_image,
        if externally_powered {
            "ac-adapter-symbolic"
        } else {
            "battery-symbolic"
        },
    );
    widgets.battery.set_text(
        &snapshot
            .shell
            .battery_percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_default(),
    );
    let power_tooltip = match (
        snapshot.shell.battery_percent,
        externally_powered,
        snapshot.shell.battery_charging,
    ) {
        (Some(percent), _, true) => format!("Battery {percent}% · charging"),
        (Some(percent), true, false) => format!("Battery {percent}% · plugged in"),
        (Some(percent), false, false) => format!("Battery {percent}% · discharging"),
        (None, _, _) => "External power".into(),
    };
    widgets
        .battery_button
        .set_tooltip_text(Some(&power_tooltip));
    set_button_active(
        &widgets.notifications_button,
        snapshot.shell.notification_unread_count > 0,
    );
    let notification_tooltip = match snapshot.shell.notification_unread_count {
        0 => "Notification center".into(),
        count => format!("Notification center: {count} unread"),
    };
    widgets
        .notifications_button
        .set_tooltip_text(Some(&notification_tooltip));
}

#[derive(Debug, PartialEq, Eq)]
struct RailDisplayStatus {
    badge: Option<&'static str>,
    warning: bool,
    tooltip: String,
}

fn rail_display_status(output: &OutputSnapshot) -> RailDisplayStatus {
    let mode = if output.hdr_active {
        "HDR active"
    } else if output.hdr_requested {
        "HDR requested but not active"
    } else {
        "SDR"
    };
    let gamut = if output.wide_gamut_active {
        "wide gamut"
    } else {
        "sRGB gamut"
    };
    let warning = (output.hdr_requested && !output.hdr_active) || output.icc_lut_fallback_active;
    let badge = if warning {
        Some("!")
    } else if output.hdr_active {
        Some("HDR")
    } else {
        None
    };
    let fallback = if output.icc_lut_fallback_active {
        " · ICC fallback active"
    } else {
        ""
    };
    RailDisplayStatus {
        badge,
        warning,
        tooltip: format!("{} · {mode} · {gamut}{fallback}", output.connector),
    }
}

fn update_clock(clock: &gtk::Label, format: ClockFormat) {
    clock.set_text(
        &chrono::Local::now()
            .format(rail_clock_pattern(format))
            .to_string(),
    );
}

fn rail_clock_pattern(format: ClockFormat) -> &'static str {
    match format {
        ClockFormat::TwelveHour => "%-I:%M\n%p",
        ClockFormat::TwentyFourHour => "%H:%M",
    }
}

/// Apply the canonical active-state class to a shell control.
pub fn set_button_active(widget: &gtk::Button, active: bool) {
    if active {
        widget.add_css_class("active");
    } else {
        widget.remove_css_class("active");
    }
}

/// Build a canonical three-layer dock module with a symbolic GTK button.
pub fn dock_button(icon_name: &str, tooltip: &str, action: impl Fn() + 'static) -> gtk::Box {
    let (button, _) = glass_icon_button(icon_name, tooltip, "dock-button", action);
    dock_module(&button)
}

fn dock_module(button: &gtk::Button) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("dock-module");
    outer.set_halign(gtk::Align::Center);
    outer.set_valign(gtk::Align::Center);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    inner.add_css_class("dock-module-inner");
    inner.set_halign(gtk::Align::Center);
    inner.set_valign(gtk::Align::Center);

    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
    inner.append(button);
    outer.append(&inner);
    outer
}

/// Build a reusable glass-styled icon button and return its image for live
/// state updates.
pub fn glass_icon_button(
    icon_name: &str,
    tooltip: &str,
    css_class: &str,
    action: impl Fn() + 'static,
) -> (gtk::Button, gtk::Image) {
    let image = shell_icon_image(icon_name);
    if css_class == "rail-button" {
        image.set_pixel_size(26);
        set_shell_icon_at_size(&image, icon_name, 26);
    } else if css_class == "shelf-button" {
        image.set_pixel_size(32);
        set_shell_icon_at_size(&image, icon_name, 32);
    }
    let button = gtk::Button::new();
    button.add_css_class(css_class);
    button.set_valign(gtk::Align::Center);
    button.set_child(Some(&image));
    button.set_tooltip_text(Some(tooltip));
    button.connect_clicked(move |_| action());
    (button, image)
}

fn shell_icon_image(icon_name: &str) -> gtk::Image {
    let image = gtk::Image::new();
    image.set_pixel_size(24);
    set_shell_icon(&image, icon_name);
    image
}

fn set_shell_icon(image: &gtk::Image, icon_name: &str) {
    set_shell_icon_at_size(image, icon_name, image.pixel_size().max(1) as u32);
}

fn set_shell_icon_at_size(image: &gtk::Image, icon_name: &str, icon_size: u32) {
    let Some(svg) = focaldesk_icon_svg(icon_name) else {
        image.set_icon_name(Some(icon_name));
        return;
    };
    let Ok(svg) = std::str::from_utf8(svg) else {
        return;
    };
    let styled = svg.replace("currentColor", "#8CA4C4");
    let mut fontdb = resvg::usvg::fontdb::Database::new();
    fontdb.load_system_fonts();
    let Ok(tree) =
        resvg::usvg::Tree::from_data(styled.as_bytes(), &resvg::usvg::Options::default(), &fontdb)
    else {
        return;
    };
    let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(icon_size, icon_size) else {
        return;
    };
    let size = tree.size();
    let icon_size_f32 = icon_size as f32;
    let transform = resvg::tiny_skia::Transform::from_scale(
        icon_size_f32 / size.width(),
        icon_size_f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let bytes = glib::Bytes::from_owned(pixmap.data().to_vec());
    let texture = gdk::MemoryTexture::new(
        icon_size as i32,
        icon_size as i32,
        gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        (icon_size * 4) as usize,
    );
    image.set_paintable(Some(&texture));
}

fn focaldesk_icon_svg(icon_name: &str) -> Option<&'static [u8]> {
    Some(match icon_name {
        "focaldesk-ai-console" => include_bytes!("../../../assets/icons/focal-ai-console.svg"),
        "preferences-system-symbolic" => include_bytes!("../../../assets/svg/settings.svg"),
        "view-app-grid-symbolic" => include_bytes!("../../../assets/svg/launcher.svg"),
        "list-add-symbolic" => include_bytes!("../../../assets/svg/plus.svg"),
        "list-remove-symbolic" => include_bytes!("../../../assets/svg/minus.svg"),
        "web-browser-symbolic" => include_bytes!("../../../assets/svg/browser.svg"),
        "utilities-terminal-symbolic" => include_bytes!("../../../assets/svg/terminal.svg"),
        "system-file-manager-symbolic" => include_bytes!("../../../assets/svg/files.svg"),
        "mail-unread-symbolic" => include_bytes!("../../../assets/svg/email.svg"),
        "network-wireless-signal-excellent-symbolic" => {
            include_bytes!("../../../assets/svg/wifi.svg")
        }
        "network-wireless-offline-symbolic" => include_bytes!("../../../assets/svg/wifi-off.svg"),
        "bluetooth-active-symbolic" => include_bytes!("../../../assets/svg/bluetooth.svg"),
        "audio-volume-high-symbolic" => include_bytes!("../../../assets/svg/volume.svg"),
        "audio-input-microphone-symbolic" => include_bytes!("../../../assets/svg/microphone.svg"),
        "microphone-sensitivity-muted-symbolic" => {
            include_bytes!("../../../assets/svg/microphone-off.svg")
        }
        "notification-symbolic" | "notification-new-symbolic" => {
            include_bytes!("../../../assets/svg/notifications.svg")
        }
        "software-update-available-symbolic" | "emblem-synchronizing-symbolic" => {
            include_bytes!("../../../assets/svg/updates.svg")
        }
        "notifications-disabled-symbolic" => include_bytes!("../../../assets/svg/volume-off.svg"),
        "camera-web-symbolic" => include_bytes!("../../../assets/svg/video.svg"),
        "camera-disabled-symbolic" => include_bytes!("../../../assets/svg/video-off.svg"),
        "video-display-symbolic" => include_bytes!("../../../assets/svg/hdr-enabled.svg"),
        "ac-adapter-symbolic" => include_bytes!("../../../assets/svg/plug.svg"),
        "battery-symbolic" => include_bytes!("../../../assets/svg/battery.svg"),
        "system-shutdown-symbolic" => include_bytes!("../../../assets/svg/power-menu.svg"),
        "focal-workspace-1" => include_bytes!("../../../assets/svg/slot-1.svg"),
        "focal-workspace-2" => include_bytes!("../../../assets/svg/slot-2.svg"),
        "focal-workspace-3" => include_bytes!("../../../assets/svg/slot-3.svg"),
        "focal-workspace-4" => include_bytes!("../../../assets/svg/slot-4.svg"),
        "focal-workspace-5" => include_bytes!("../../../assets/svg/slot-5.svg"),
        "focal-workspace-6" => include_bytes!("../../../assets/svg/slot-6.svg"),
        "focal-workspace-7" => include_bytes!("../../../assets/svg/slot-7.svg"),
        "focal-workspace-8" => include_bytes!("../../../assets/svg/slot-8.svg"),
        "focal-workspace-9" => include_bytes!("../../../assets/svg/slot-9.svg"),
        _ => return None,
    })
}

fn desktop_snapshot() -> Option<DesktopSnapshot> {
    match send_desktop_request(&IpcRequest::GetDesktopSnapshot) {
        Ok(IpcResponse::DesktopSnapshot { snapshot }) => Some(snapshot),
        Ok(_) => None,
        Err(error) => {
            flog(format!("shell snapshot failed: {error}"));
            None
        }
    }
}

fn start_snapshot_poll(
    window: &gtk::ApplicationWindow,
    interval: Duration,
) -> Arc<Mutex<Option<DesktopSnapshot>>> {
    let latest = Arc::new(Mutex::new(None));
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_on_close = stopped.clone();
    window.connect_close_request(move |_| {
        stopped_on_close.store(true, Ordering::Release);
        glib::Propagation::Proceed
    });

    let latest_for_thread = latest.clone();
    thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            if let Some(snapshot) = desktop_snapshot() {
                if let Ok(mut slot) = latest_for_thread.lock() {
                    *slot = Some(snapshot);
                }
            }
            thread::sleep(interval);
        }
    });
    latest
}

fn output_for_connector<'a>(
    snapshot: &'a DesktopSnapshot,
    connector: &str,
) -> Option<&'a focaldesk_ipc::OutputSnapshot> {
    snapshot
        .outputs
        .iter()
        .find(|output| output.connector == connector)
}

fn send_action(action: DesktopAction) {
    thread::spawn(move || {
        if let Err(error) = send_desktop_request(&IpcRequest::ExecuteDesktopAction { action }) {
            flog(format!("shell action failed: {error}"));
        }
    });
}

fn open_shell_panel(connector: &str, panel: ShellPanel) {
    send_action(DesktopAction::OpenShellPanel {
        connector: connector.to_string(),
        panel,
    });
}

fn install_theme() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    let initial = active_theme_snapshot();
    apply_theme_snapshot(&provider, &initial);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let current = Rc::new(RefCell::new(initial));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let next = active_theme_snapshot();
        if next != *current.borrow() {
            apply_theme_snapshot(&provider, &next);
            *current.borrow_mut() = next;
        }
        glib::ControlFlow::Continue
    });
}

#[derive(Clone, Debug, PartialEq)]
struct ThemeSnapshot {
    name: String,
    font_scale: f64,
    animations: bool,
    shell_style: ShellStyle,
    panel_position: PanelPosition,
    panel_corner_radius: f64,
    dock_position: DockPosition,
    dock_corner_radius: f64,
    dock_size: DockSize,
}

fn active_theme_snapshot() -> ThemeSnapshot {
    let config = load_config();
    let settings = load_settings();
    ThemeSnapshot {
        name: config.appearance.theme,
        font_scale: config.appearance.font_scale,
        animations: settings.appearance.animations,
        shell_style: config.shell.style,
        panel_position: config.panel.position,
        panel_corner_radius: config.panel.corner_radius,
        dock_position: config.dock.position,
        dock_corner_radius: config.dock.corner_radius,
        dock_size: config.dock.size,
    }
}

fn apply_theme_snapshot(provider: &gtk::CssProvider, snapshot: &ThemeSnapshot) {
    let theme = theme_by_name(&snapshot.name);
    provider.load_from_string(&shell_css_configured(&theme, snapshot));
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(snapshot.animations);
    }
}

/// Generate the GTK shell stylesheet from shared FocalDesk theme tokens.
pub fn shell_css(theme: &FlowTheme, font_scale: f64) -> String {
    let defaults = FocalDeskConfig::default();
    shell_css_configured(
        theme,
        &ThemeSnapshot {
            name: defaults.appearance.theme,
            font_scale,
            animations: true,
            shell_style: defaults.shell.style,
            panel_position: defaults.panel.position,
            panel_corner_radius: defaults.panel.corner_radius,
            dock_position: defaults.dock.position,
            dock_corner_radius: defaults.dock.corner_radius,
            dock_size: defaults.dock.size,
        },
    )
}

fn shell_css_configured(theme: &FlowTheme, snapshot: &ThemeSnapshot) -> String {
    let background = theme.chrome.bg_color;
    let surface = theme.chrome.panel_color;
    let accent = theme.chrome.accent_color;
    let trim = theme.chrome.trim_color;
    let definitions = [
        ("fd_shell_bg", background),
        ("fd_shell_surface", surface),
        (
            "fd_shell_surface_raised",
            mix(surface, theme.text.normal, 0.08),
        ),
        ("fd_shell_glass_top", mix(surface, theme.text.normal, 0.15)),
        ("fd_shell_glass_mid", mix(surface, accent, 0.08)),
        (
            "fd_shell_glass_bottom",
            mix(surface, [0.0, 0.0, 0.0, 1.0], 0.30),
        ),
        ("fd_shell_surface_hover", mix(surface, accent, 0.20)),
        ("fd_shell_surface_active", mix(surface, accent, 0.32)),
        ("fd_shell_border", trim),
        ("fd_shell_border_soft", with_alpha(trim, 0.62)),
        ("fd_shell_rim", mix(trim, theme.text.normal, 0.18)),
        ("fd_shell_highlight", with_alpha(theme.text.normal, 0.26)),
        ("fd_shell_accent", accent),
        ("fd_shell_text", theme.text.normal),
        ("fd_shell_text_dim", theme.text.dim),
        ("fd_shell_clock", theme.text.clock),
        ("fd_shell_icon", theme.icons.inactive),
        ("fd_shell_dock_icon", with_alpha(theme.icons.inactive, 1.0)),
        ("fd_shell_icon_hover", theme.icons.hover),
        ("fd_shell_icon_active", theme.icons.active),
        (
            "fd_shell_shadow",
            [0.0, 0.0, 0.0, theme.chrome.shadow_intensity.clamp(0.0, 1.0)],
        ),
        (
            "fd_shell_glow",
            with_alpha(
                theme.icons.glow,
                theme.chrome.glow_intensity.clamp(0.0, 1.0),
            ),
        ),
    ]
    .into_iter()
    .map(|(name, color)| format!("@define-color {name} {};", rgba(color)))
    .collect::<Vec<_>>()
    .join("\n");
    let radius = theme.chrome.corner_radius.max(3.0);
    let border_width = theme.chrome.border_width.max(1.0);
    let transition_ms = if snapshot.animations {
        (150.0 / theme.animation_speed.max(0.1)).round() as u32
    } else {
        0
    };
    let font_scale = snapshot.font_scale.clamp(0.75, 1.5);
    let panel_radius = snapshot.panel_corner_radius.clamp(0.0, 48.0);
    let dock_radius = snapshot.dock_corner_radius.clamp(0.0, 48.0);
    let dock_control_size = match snapshot.dock_size {
        DockSize::Compact => 36,
        DockSize::Normal => DEFAULT_METRICS.control_size,
        DockSize::Expanded => 50,
    };
    let panel_corners = match (snapshot.shell_style, snapshot.panel_position) {
        (ShellStyle::Floating, _) => format!("{panel_radius:.1}px"),
        (ShellStyle::Attached, PanelPosition::Top) => {
            format!("0 0 {panel_radius:.1}px {panel_radius:.1}px")
        }
        (ShellStyle::Attached, PanelPosition::Bottom) => {
            format!("{panel_radius:.1}px {panel_radius:.1}px 0 0")
        }
    };
    let dock_corners = match (snapshot.shell_style, snapshot.dock_position) {
        (ShellStyle::Floating, _) => format!("{dock_radius:.1}px"),
        (ShellStyle::Attached, DockPosition::Left) => {
            format!("0 {dock_radius:.1}px {dock_radius:.1}px 0")
        }
        (ShellStyle::Attached, DockPosition::Right) => {
            format!("{dock_radius:.1}px 0 0 {dock_radius:.1}px")
        }
    };
    let panel_edge = match snapshot.panel_position {
        PanelPosition::Top => "border-bottom: 1px solid @fd_shell_border; border-top: none;",
        PanelPosition::Bottom => "border-top: 1px solid @fd_shell_border; border-bottom: none;",
    };

    format!(
         "{definitions}\n{SHELL_CSS_BASE}\n\
         window.focal-shell-window {{ font-size: {font_scale:.3}em; }}\n\
         window.focal-panel-window > .focal-panel {{ border-radius: {panel_corners}; {panel_edge} }}\n\
         .focal-dock {{ border-radius: {dock_corners}; border-width: {border_width:.1}px; }}\n\
         .dock-module {{ min-height: {dock_control_size}px; border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         .panel-launcher, .panel-well, .panel-status-button {{ border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         window.focal-shell-window * {{ transition-duration: {transition_ms}ms; }}\n"
    )
}

fn mix(left: [f32; 4], right: [f32; 4], amount: f32) -> [f32; 4] {
    let amount = amount.clamp(0.0, 1.0);
    [
        left[0] + (right[0] - left[0]) * amount,
        left[1] + (right[1] - left[1]) * amount,
        left[2] + (right[2] - left[2]) * amount,
        left[3] + (right[3] - left[3]) * amount,
    ]
}

fn with_alpha(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] = alpha;
    color
}

fn rgba(color: [f32; 4]) -> String {
    format!(
        "rgba({:.0}, {:.0}, {:.0}, {:.3})",
        color[0].clamp(0.0, 1.0) * 255.0,
        color[1].clamp(0.0, 1.0) * 255.0,
        color[2].clamp(0.0, 1.0) * 255.0,
        color[3].clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        email_identity_matches, rail_clock_pattern, rail_display_status, running_app_capacity,
        shelf_window_label, shell_css, shell_css_configured, window_belongs_to_output,
        window_overlaps_shelf, ShelfOverflowEntry, ShellRole, ThemeSnapshot,
    };
    use focaldesk_config::{
        ClockFormat, DockPosition, DockSize, FocalDeskConfig, PanelPosition, ShellStyle,
    };
    use focaldesk_ipc::{OutputSnapshot, WindowSnapshot};
    use focaldesk_themes::theme_by_name;

    #[test]
    fn shell_css_tracks_all_builtin_theme_palettes() {
        let eagle = shell_css(&theme_by_name("Eagle"), 1.0);
        let moonbase = shell_css(&theme_by_name("Moonbase"), 1.0);
        let classic = shell_css(&theme_by_name("Classic"), 1.0);

        assert!(eagle.contains("rgba(51, 153, 255"));
        assert!(moonbase.contains("rgba(107, 173, 219"));
        assert!(classic.contains("rgba(255, 128, 0"));
        assert!(eagle.contains("@define-color fd_shell_dock_icon rgba(178, 191, 209, 1.000);"));
        assert!(eagle.contains("color: @fd_shell_dock_icon;"));
        assert!(eagle.contains(".focal-dock { border-radius:"));
        assert_ne!(eagle, moonbase);
        assert_ne!(moonbase, classic);
    }

    #[test]
    fn shell_css_applies_edge_corners_and_compact_dock_metrics() {
        let css = shell_css_configured(
            &theme_by_name("Eagle"),
            &ThemeSnapshot {
                name: "Eagle".into(),
                font_scale: 1.0,
                animations: true,
                shell_style: ShellStyle::Attached,
                panel_position: PanelPosition::Top,
                panel_corner_radius: 16.0,
                dock_position: DockPosition::Left,
                dock_corner_radius: 24.0,
                dock_size: DockSize::Compact,
            },
        );

        assert!(css.contains("border-radius: 0 0 16.0px 16.0px"));
        assert!(css.contains("border-radius: 0 24.0px 24.0px 0"));
        assert!(css.contains("min-height: 36px"));
        assert!(css.contains(".shell-surface"));
    }

    #[test]
    fn shell_css_disables_transitions_when_motion_is_reduced() {
        let defaults = FocalDeskConfig::default();
        let css = shell_css_configured(
            &theme_by_name("Eagle"),
            &ThemeSnapshot {
                name: "Eagle".into(),
                font_scale: 1.0,
                animations: false,
                shell_style: defaults.shell.style,
                panel_position: defaults.panel.position,
                panel_corner_radius: defaults.panel.corner_radius,
                dock_position: defaults.dock.position,
                dock_corner_radius: defaults.dock.corner_radius,
                dock_size: defaults.dock.size,
            },
        );

        assert!(css.contains("transition-duration: 0ms"));
    }

    #[test]
    fn clock_format_selects_twelve_or_twenty_four_hour_time() {
        assert_eq!(rail_clock_pattern(ClockFormat::TwelveHour), "%-I:%M\n%p");
        assert_eq!(rail_clock_pattern(ClockFormat::TwentyFourHour), "%H:%M");
    }

    #[test]
    fn layer_shell_uses_zero_for_each_compositor_sized_axis() {
        assert_eq!(ShellRole::Panel.layer_default_size(), (64, 0));
        assert_eq!(ShellRole::Dock.layer_default_size(), (560, 64));
    }

    #[test]
    fn shelf_assigns_unassociated_windows_by_geometry_then_focus() {
        let left = output_snapshot(1, 0, true);
        let right = output_snapshot(2, 1920, false);
        let mut window = window_snapshot();
        window.x = Some(2200);
        window.y = Some(100);
        window.width = Some(800);
        window.height = Some(600);

        assert!(!window_belongs_to_output(&window, Some(&left), 2));
        assert!(window_belongs_to_output(&window, Some(&right), 2));

        window.x = None;
        window.y = None;
        window.width = None;
        window.height = None;
        window.focused = true;
        assert!(window_belongs_to_output(&window, Some(&left), 2));
        assert!(!window_belongs_to_output(&window, Some(&right), 2));
    }

    #[test]
    fn display_badge_only_calls_attention_to_hdr_or_fallback() {
        let mut output = output_snapshot(1, 0, true);
        let sdr = rail_display_status(&output);
        assert_eq!(sdr.badge, None);
        assert!(sdr.tooltip.contains("SDR"));

        output.hdr_requested = true;
        output.hdr_active = true;
        output.wide_gamut_active = true;
        let hdr = rail_display_status(&output);
        assert_eq!(hdr.badge, Some("HDR"));
        assert!(!hdr.warning);
        assert!(hdr.tooltip.contains("HDR active · wide gamut"));

        output.hdr_active = false;
        output.icc_lut_fallback_active = true;
        let fallback = rail_display_status(&output);
        assert_eq!(fallback.badge, Some("!"));
        assert!(fallback.warning);
        assert!(fallback.tooltip.contains("ICC fallback active"));
    }

    #[test]
    fn intelligent_dodge_only_triggers_for_shelf_overlap() {
        let output = output_snapshot(1, 0, true);
        let mut window = window_snapshot();
        window.x = Some(800);
        window.y = Some(950);
        window.width = Some(320);
        window.height = Some(120);
        assert!(window_overlaps_shelf(&window, &output));

        window.x = Some(20);
        window.y = Some(20);
        window.width = Some(400);
        window.height = Some(300);
        assert!(!window_overlaps_shelf(&window, &output));
    }

    #[test]
    fn shelf_keeps_pinned_apps_and_reserves_a_real_overflow_button() {
        assert_eq!(running_app_capacity(4, false), 4);
        assert_eq!(running_app_capacity(4, true), 3);
        assert_eq!(running_app_capacity(3, false), 5);
        assert_eq!(running_app_capacity(3, true), 4);
    }

    #[test]
    fn configured_mail_client_matches_its_existing_window() {
        assert!(email_identity_matches(
            "thunderbird",
            "org.mozilla.Thunderbird"
        ));
        assert!(email_identity_matches(
            "flatpak run org.gnome.Evolution",
            "org.gnome.Evolution"
        ));
        assert!(!email_identity_matches(
            "thunderbird",
            "org.mozilla.firefox"
        ));
    }

    #[test]
    fn chooser_never_repeats_an_unhelpful_untitled_label() {
        let entry = ShelfOverflowEntry {
            id: 7,
            identity: "untitled".into(),
            title: "Untitled".into(),
            focused: false,
        };
        assert_eq!(shelf_window_label(&entry, 0), "Window 1");
        assert_eq!(shelf_window_label(&entry, 18), "Window 19");

        let identified = ShelfOverflowEntry {
            identity: "org.gnome.TextEditor".into(),
            ..entry
        };
        assert_eq!(shelf_window_label(&identified, 1), "TextEditor · Window 2");
    }

    fn output_snapshot(id: u64, x: i32, focused: bool) -> OutputSnapshot {
        OutputSnapshot {
            id,
            connector: format!("output-{id}"),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            width: 1920,
            height: 1080,
            x,
            y: 0,
            scale: 1.0,
            active_workspace_id: 1,
            focused,
            hdr_supported: false,
            hdr_requested: false,
            hdr_active: false,
            wide_gamut_active: false,
            icc_lut_fallback_active: false,
        }
    }

    fn window_snapshot() -> WindowSnapshot {
        WindowSnapshot {
            id: 1,
            title: "Terminal".into(),
            app_id: Some("terminal".into()),
            class: None,
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
        }
    }
}
