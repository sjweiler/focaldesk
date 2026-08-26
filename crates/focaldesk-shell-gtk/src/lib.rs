//! Reusable GTK4 shell toolkit for the FocalDesk panel and dock clients.
//!
//! This crate is a Wayland client. It consumes renderer-neutral theme tokens,
//! but intentionally has no dependency on `focaldesk-ui` or any compositor
//! renderer, shader, or icon atlas.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use anyhow::Result;
use focaldesk_config::{
    load_config, DockPosition, DockSize, FocalDeskConfig, PanelPosition, ShellStyle,
};
use focaldesk_ipc::{
    send_desktop_request, DesktopAction, DesktopSnapshot, IpcRequest, IpcResponse,
};
use focaldesk_logging::{flog, init_default_logging, startup_banner};
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
    control_size: 44,
    control_gap: 8,
};

const PANEL_HEIGHT: i32 = DEFAULT_METRICS.panel_height;
const DOCK_WIDTH: i32 = DEFAULT_METRICS.dock_width;
const SHELL_CSS_BASE: &str = include_str!("shell.css");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRole {
    Panel,
    Dock,
}

impl ShellRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Panel => "focal-panel",
            Self::Dock => "focal-dock",
        }
    }

    const fn application_id(self) -> &'static str {
        match self {
            Self::Panel => "dev.focaldesk.Panel",
            Self::Dock => "dev.focaldesk.Dock",
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
        let window = match role {
            ShellRole::Dock => build_dock(app, config),
            ShellRole::Panel => build_panel(app, index + 1, config),
        };
        configure_layer_window(&window, role, &monitor, config);
        window.present();
        output_count += 1;
    }

    if output_count == 0 {
        let window = match role {
            ShellRole::Dock => build_dock(app, config),
            ShellRole::Panel => build_panel(app, 1, config),
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
    config: &FocalDeskConfig,
) {
    window.init_layer_shell();
    window.set_namespace(role.namespace());
    window.set_layer(Layer::Top);
    window.set_keyboard_mode(KeyboardMode::None);
    window.add_css_class(match config.shell.style {
        ShellStyle::Floating => "floating",
        ShellStyle::Attached => "attached",
    });
    match role {
        ShellRole::Panel => {
            let edge = match config.panel.position {
                PanelPosition::Top => Edge::Top,
                PanelPosition::Bottom => Edge::Bottom,
            };
            window.add_css_class(match config.panel.position {
                PanelPosition::Top => "edge-top",
                PanelPosition::Bottom => "edge-bottom",
            });
            window.set_anchor(edge, true);
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
            window.set_default_size(1, PANEL_HEIGHT);
            if config.shell.style == ShellStyle::Floating {
                window.set_margin(edge, 10);
                window.set_margin(Edge::Left, 12);
                window.set_margin(Edge::Right, 12);
                window.set_exclusive_zone(PANEL_HEIGHT + 10);
            } else {
                window.set_exclusive_zone(PANEL_HEIGHT);
            }
        }
        ShellRole::Dock => {
            let edge = match config.dock.position {
                DockPosition::Left => Edge::Left,
                DockPosition::Right => Edge::Right,
            };
            window.add_css_class(match config.dock.position {
                DockPosition::Left => "edge-left",
                DockPosition::Right => "edge-right",
            });
            window.set_anchor(edge, true);
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Bottom, true);
            window.set_default_size(DOCK_WIDTH, 1);
            if config.shell.style == ShellStyle::Floating {
                window.set_margin(edge, 12);
                window.set_margin(Edge::Top, 10);
                window.set_margin(Edge::Bottom, 10);
                window.set_exclusive_zone(0);
            } else {
                window.set_exclusive_zone(DOCK_WIDTH);
            }
        }
    }
}

#[derive(Default)]
struct DockWidgets {
    workspace_count: usize,
    workspace_buttons: Vec<gtk::Button>,
}

fn build_dock(app: &gtk::Application, config: &FocalDeskConfig) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("focal-shell-window");
    window.add_css_class("focal-dock-window");

    let gap = match config.dock.size {
        DockSize::Compact => 5,
        DockSize::Normal => 8,
        DockSize::Expanded => 10,
    };
    let padding = match config.dock.size {
        DockSize::Compact => 6,
        DockSize::Normal => 8,
        DockSize::Expanded => 10,
    };
    let rail = gtk::Box::new(gtk::Orientation::Vertical, gap);
    rail.add_css_class("focal-dock");
    rail.set_margin_top(padding);
    rail.set_margin_bottom(padding);
    rail.set_margin_start(padding);
    rail.set_margin_end(padding);
    window.set_child(Some(&rail));

    rail.append(&dock_button(
        "preferences-system-symbolic",
        "Settings",
        || {
            send_action(DesktopAction::OpenSettingsPanel {
                panel: "appearance".into(),
            })
        },
    ));
    rail.append(&dock_button("view-app-grid-symbolic", "Launcher", || {
        send_action(DesktopAction::LaunchApp {
            app: "@launcher".into(),
        });
    }));

    let workspace_box = gtk::Box::new(gtk::Orientation::Vertical, gap);
    workspace_box.add_css_class("dock-workspaces");
    rail.append(&workspace_box);

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.add_css_class("dock-separator");
    rail.append(&separator);

    rail.append(&dock_button(
        "list-add-symbolic",
        "Add new workspace",
        || send_action(DesktopAction::CreateWorkspace),
    ));
    rail.append(&dock_button(
        "list-remove-symbolic",
        "Delete workspace",
        || send_action(DesktopAction::DeleteWorkspace),
    ));
    rail.append(&dock_button("web-browser-symbolic", "Browser", || {
        send_action(DesktopAction::LaunchApp {
            app: "@browser".into(),
        });
    }));
    rail.append(&dock_button(
        "utilities-terminal-symbolic",
        "Terminal",
        || {
            send_action(DesktopAction::LaunchApp {
                app: "@terminal".into(),
            });
        },
    ));
    rail.append(&dock_button(
        "system-file-manager-symbolic",
        "Files",
        || {
            send_action(DesktopAction::LaunchApp {
                app: "@files".into(),
            });
        },
    ));
    rail.append(&dock_button("mail-unread-symbolic", "Email", || {
        send_action(DesktopAction::LaunchApp {
            app: "evolution".into(),
        });
    }));

    let widgets = Rc::new(RefCell::new(DockWidgets::default()));
    rebuild_workspace_buttons(&workspace_box, &widgets, 1);
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let Some(_window) = weak_window.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if let Some(snapshot) = desktop_snapshot() {
            let count = snapshot.shell.workspace_count.max(1);
            if widgets.borrow().workspace_count != count {
                rebuild_workspace_buttons(&workspace_box, &widgets, count);
            }
            for (index, button) in widgets.borrow().workspace_buttons.iter().enumerate() {
                if snapshot.session.active_workspace_id == index as u32 + 1 {
                    button.add_css_class("active");
                } else {
                    button.remove_css_class("active");
                }
            }
        }
        glib::ControlFlow::Continue
    });

    window
}

fn rebuild_workspace_buttons(
    workspace_box: &gtk::Box,
    widgets: &Rc<RefCell<DockWidgets>>,
    workspace_count: usize,
) {
    while let Some(child) = workspace_box.first_child() {
        workspace_box.remove(&child);
    }
    let mut buttons = Vec::new();
    for workspace in 1..=workspace_count.min(9) {
        let button = gtk::Button::with_label(&workspace.to_string());
        button.add_css_class("dock-button");
        button.add_css_class("workspace-button");
        button.set_halign(gtk::Align::Center);
        button.set_tooltip_text(Some(&format!("Workspace {workspace}")));
        button.connect_clicked(move |_| {
            send_action(DesktopAction::FocusWorkspace {
                workspace: workspace as u32,
            });
        });
        workspace_box.append(&button);
        buttons.push(button);
    }
    *widgets.borrow_mut() = DockWidgets {
        workspace_count,
        workspace_buttons: buttons,
    };
}

struct PanelWidgets {
    workspace: gtk::Label,
    workspace_menu: gtk::Box,
    workspace_popover: gtk::Popover,
    workspace_count: Cell<usize>,
    active_workspace: Cell<u32>,
    title: gtk::Label,
    network_button: gtk::Button,
    network_image: gtk::Image,
    notifications_button: gtk::Button,
    notifications_image: gtk::Image,
    updates_button: gtk::Button,
    updates_image: gtk::Image,
    dnd_button: gtk::Button,
    dnd_image: gtk::Image,
    battery: gtk::Label,
    clock: gtk::Label,
}

fn build_panel(
    app: &gtk::Application,
    output_number: u32,
    _config: &FocalDeskConfig,
) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("focal-shell-window");
    window.add_css_class("focal-panel-window");

    let panel = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    panel.add_css_class("focal-panel");
    panel.set_height_request(PANEL_HEIGHT);
    window.set_child(Some(&panel));

    let corner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    corner.add_css_class("panel-corner");
    corner.set_width_request(DOCK_WIDTH);
    panel.append(&corner);

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
    panel.append(&launcher);

    let identity = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    identity.add_css_class("panel-identity");
    let brand = gtk::Label::new(Some("FOCALDESK"));
    brand.add_css_class("panel-brand");
    identity.append(&brand);
    let output_label = gtk::Label::new(Some(&format!("OUT {output_number}")));
    output_label.add_css_class("panel-meta");
    identity.append(&output_label);
    let workspace = gtk::Label::new(Some("Workspace 1"));
    workspace.add_css_class("panel-meta");
    let workspace_button = gtk::MenuButton::new();
    workspace_button.add_css_class("panel-workspace-button");
    workspace_button.set_tooltip_text(Some("Choose workspace"));
    workspace_button.set_child(Some(&workspace));
    let workspace_popover = gtk::Popover::new();
    workspace_popover.add_css_class("workspace-popover");
    let workspace_menu = gtk::Box::new(gtk::Orientation::Vertical, 4);
    workspace_menu.set_margin_top(8);
    workspace_menu.set_margin_bottom(8);
    workspace_menu.set_margin_start(8);
    workspace_menu.set_margin_end(8);
    workspace_popover.set_child(Some(&workspace_menu));
    workspace_button.set_popover(Some(&workspace_popover));
    identity.append(&workspace_button);
    panel.append(&identity);

    let title = gtk::Label::new(None);
    title.add_css_class("panel-title");
    title.add_css_class("panel-well");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    title.set_valign(gtk::Align::Center);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    panel.append(&title);

    let status_cluster = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    status_cluster.add_css_class("panel-status-cluster");

    let (network_button, network_image) = glass_icon_button(
        "network-wireless-offline-symbolic",
        "Network",
        "panel-status-button",
        || {
            send_action(DesktopAction::OpenSettingsPanel {
                panel: "network".into(),
            });
        },
    );
    status_cluster.append(&network_button);
    status_cluster.append(
        &glass_icon_button(
            "bluetooth-active-symbolic",
            "Bluetooth",
            "panel-status-button",
            || {
                send_action(DesktopAction::OpenSettingsPanel {
                    panel: "bluetooth".into(),
                });
            },
        )
        .0,
    );
    status_cluster.append(
        &glass_icon_button(
            "audio-volume-high-symbolic",
            "Sound",
            "panel-status-button",
            || {
                send_action(DesktopAction::OpenSettingsPanel {
                    panel: "sound".into(),
                });
            },
        )
        .0,
    );
    let (notifications_button, notifications_image) = glass_icon_button(
        "notification-symbolic",
        "Notifications",
        "panel-status-button",
        || send_action(DesktopAction::OpenNotificationsPanel),
    );
    status_cluster.append(&notifications_button);
    let (updates_button, updates_image) = glass_icon_button(
        "software-update-available-symbolic",
        "System updates",
        "panel-status-button",
        || send_action(DesktopAction::OpenUpdatesPanel),
    );
    status_cluster.append(&updates_button);
    let (dnd_button, dnd_image) = glass_icon_button(
        "notifications-disabled-symbolic",
        "Do Not Disturb",
        "panel-status-button",
        || send_action(DesktopAction::ToggleDoNotDisturb),
    );
    status_cluster.append(&dnd_button);
    status_cluster.append(
        &glass_icon_button(
            "video-display-symbolic",
            "Displays",
            "panel-status-button",
            || {
                send_action(DesktopAction::OpenSettingsPanel {
                    panel: "displays".into(),
                });
            },
        )
        .0,
    );
    let battery = gtk::Label::new(None);
    battery.add_css_class("panel-battery");
    battery.add_css_class("panel-well");
    status_cluster.append(&battery);
    status_cluster.append(
        &glass_icon_button(
            "system-shutdown-symbolic",
            "Power",
            "panel-status-button",
            || {
                send_action(DesktopAction::OpenSettingsPanel {
                    panel: "power".into(),
                });
            },
        )
        .0,
    );
    panel.append(&status_cluster);

    let clock = gtk::Label::new(None);
    clock.add_css_class("panel-clock");
    let clock_button = gtk::Button::new();
    clock_button.add_css_class("panel-well");
    clock_button.set_tooltip_text(Some("Calendar and clock"));
    clock_button.set_child(Some(&clock));
    clock_button.connect_clicked(|_| send_action(DesktopAction::OpenCalendarPanel));
    panel.append(&clock_button);

    let widgets = PanelWidgets {
        workspace,
        workspace_menu,
        workspace_popover,
        workspace_count: Cell::new(0),
        active_workspace: Cell::new(0),
        title,
        network_button,
        network_image,
        notifications_button,
        notifications_image,
        updates_button,
        updates_image,
        dnd_button,
        dnd_image,
        battery,
        clock,
    };
    rebuild_panel_workspace_menu(&widgets.workspace_menu, &widgets.workspace_popover, 1, 1);
    widgets.workspace_count.set(1);
    widgets.active_workspace.set(1);
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let Some(_window) = weak_window.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if let Some(snapshot) = desktop_snapshot() {
            update_panel(&widgets, &snapshot);
        } else {
            widgets
                .clock
                .set_text(&chrono::Local::now().format("%-I:%M %p").to_string());
        }
        glib::ControlFlow::Continue
    });

    window
}

fn update_panel(widgets: &PanelWidgets, snapshot: &DesktopSnapshot) {
    widgets.workspace.set_text(&format!(
        "Workspace {}",
        snapshot.session.active_workspace_id
    ));
    let workspace_count = snapshot.shell.workspace_count.max(1);
    let active_workspace = snapshot.session.active_workspace_id.max(1);
    if widgets.workspace_count.get() != workspace_count
        || widgets.active_workspace.get() != active_workspace
    {
        rebuild_panel_workspace_menu(
            &widgets.workspace_menu,
            &widgets.workspace_popover,
            workspace_count,
            active_workspace,
        );
        widgets.workspace_count.set(workspace_count);
        widgets.active_workspace.set(active_workspace);
    }
    widgets
        .title
        .set_text(snapshot.shell.focused_window_title.as_deref().unwrap_or(""));
    widgets
        .network_image
        .set_icon_name(Some(if snapshot.shell.network_carrier {
            "network-wireless-signal-excellent-symbolic"
        } else {
            "network-wireless-offline-symbolic"
        }));
    set_button_active(&widgets.network_button, snapshot.shell.network_carrier);
    widgets.notifications_image.set_icon_name(Some(
        if snapshot.shell.notification_unread_count > 0 {
            "notification-new-symbolic"
        } else {
            "notification-symbolic"
        },
    ));
    set_button_active(
        &widgets.notifications_button,
        snapshot.shell.notification_unread_count > 0,
    );
    widgets
        .updates_image
        .set_icon_name(Some(if snapshot.shell.update_busy {
            "emblem-synchronizing-symbolic"
        } else if snapshot.shell.update_available_count > 0 {
            "software-update-available-symbolic"
        } else {
            "software-update-available-symbolic"
        }));
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
    widgets
        .dnd_image
        .set_icon_name(Some(if snapshot.shell.do_not_disturb {
            "notifications-disabled-symbolic"
        } else {
            "notifications-symbolic"
        }));
    set_button_active(&widgets.dnd_button, snapshot.shell.do_not_disturb);
    widgets.battery.set_text(
        &snapshot
            .shell
            .battery_percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_default(),
    );
    widgets
        .clock
        .set_text(&chrono::Local::now().format("%-I:%M %p").to_string());
}

fn rebuild_panel_workspace_menu(
    menu: &gtk::Box,
    popover: &gtk::Popover,
    workspace_count: usize,
    active_workspace: u32,
) {
    while let Some(child) = menu.first_child() {
        menu.remove(&child);
    }

    let heading = gtk::Label::new(Some(&format!("Current: Workspace {active_workspace}")));
    heading.add_css_class("workspace-menu-heading");
    heading.set_halign(gtk::Align::Start);
    menu.append(&heading);
    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    for workspace in 1..=workspace_count.min(9) {
        let prefix = if workspace as u32 == active_workspace {
            "✓"
        } else {
            " "
        };
        let label = gtk::Label::new(Some(&format!("{prefix}  Workspace {workspace}")));
        label.set_xalign(0.0);
        let button = gtk::Button::new();
        button.set_child(Some(&label));
        button.add_css_class("workspace-menu-item");
        button.set_halign(gtk::Align::Fill);
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            send_action(DesktopAction::FocusWorkspace {
                workspace: workspace as u32,
            });
            popover.popdown();
        });
        menu.append(&button);
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

/// Build a canonical dock button with a symbolic GTK icon.
pub fn dock_button(icon_name: &str, tooltip: &str, action: impl Fn() + 'static) -> gtk::Button {
    let (button, _) = glass_icon_button(icon_name, tooltip, "dock-button", action);
    button.set_halign(gtk::Align::Center);
    button
}

/// Build a reusable glass-styled icon button and return its image for live
/// state updates.
pub fn glass_icon_button(
    icon_name: &str,
    tooltip: &str,
    css_class: &str,
    action: impl Fn() + 'static,
) -> (gtk::Button, gtk::Image) {
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size(24);
    let button = gtk::Button::new();
    button.add_css_class(css_class);
    button.set_child(Some(&image));
    button.set_tooltip_text(Some(tooltip));
    button.connect_clicked(move |_| action());
    (button, image)
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

fn send_action(action: DesktopAction) {
    if let Err(error) = send_desktop_request(&IpcRequest::ExecuteDesktopAction { action }) {
        flog(format!("shell action failed: {error}"));
    }
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
    shell_style: ShellStyle,
    panel_position: PanelPosition,
    panel_corner_radius: f64,
    dock_position: DockPosition,
    dock_corner_radius: f64,
    dock_size: DockSize,
}

fn active_theme_snapshot() -> ThemeSnapshot {
    let config = load_config();
    ThemeSnapshot {
        name: config.appearance.theme,
        font_scale: config.appearance.font_scale,
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
}

/// Generate the GTK shell stylesheet from shared FocalDesk theme tokens.
pub fn shell_css(theme: &FlowTheme, font_scale: f64) -> String {
    let defaults = FocalDeskConfig::default();
    shell_css_configured(
        theme,
        &ThemeSnapshot {
            name: defaults.appearance.theme,
            font_scale,
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
    let transition_ms = (150.0 / theme.animation_speed.max(0.1)).round() as u32;
    let font_scale = snapshot.font_scale.clamp(0.75, 1.5);
    let panel_radius = snapshot.panel_corner_radius.clamp(0.0, 48.0);
    let dock_radius = snapshot.dock_corner_radius.clamp(0.0, 48.0);
    let dock_control_size = match snapshot.dock_size {
        DockSize::Compact => 38,
        DockSize::Normal => DEFAULT_METRICS.control_size,
        DockSize::Expanded => 52,
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
         .dock-button {{ min-width: {dock_control_size}px; min-height: {dock_control_size}px; border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         .panel-launcher, .panel-identity, .panel-well, .panel-status-button, .panel-workspace-button {{ border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         popover.workspace-popover > contents {{ border-radius: {radius:.1}px; }}\n\
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
    use super::{shell_css, shell_css_configured, ThemeSnapshot};
    use focaldesk_config::{DockPosition, DockSize, PanelPosition, ShellStyle};
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
        assert!(css.contains("min-width: 38px; min-height: 38px"));
    }
}
