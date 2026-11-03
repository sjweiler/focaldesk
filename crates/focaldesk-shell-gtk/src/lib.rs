//! Reusable GTK4 shell toolkit for the FocalDesk panel and dock clients.
//!
//! This crate is a Wayland client. It consumes renderer-neutral theme tokens,
//! but intentionally has no dependency on `focaldesk-ui` or any compositor
//! renderer, shader, or icon atlas.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    process::Command,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use focaldesk_ai::{send_ai_request, AiIpcRequest, AiIpcResponse, ChatRequest};
use focaldesk_config::{
    load_config, ClockFormat, DockPosition, DockSize, DockVisibility, FocalDeskConfig,
    PanelPosition, ShellStyle,
};
use focaldesk_ipc::{
    send_desktop_request, send_microphone_request, send_power_request, send_speech_request,
    DesktopAction, DesktopSnapshot, IpcRequest, IpcResponse, MicrophoneEvent, MicrophoneIpcRequest,
    OutputSnapshot, PowerIpcRequest, PowerIpcResponse, ShellPanel, SpeechCommand, SpeechIpcRequest,
    WindowSnapshot,
};
use focaldesk_logging::{flog, init_default_logging, startup_banner};
use focaldesk_settings_core::{load_settings, save_settings, DisplayTransform, OutputRole};
use focaldesk_themes::{theme_by_name, FlowTheme};
use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use zbus::{
    blocking::{Connection as DbusConnection, Proxy as DbusProxy},
    zvariant::OwnedValue,
};

mod vui_client;

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
// Desktop IPC does not yet expose a snapshot-change subscription. Keep this
// bounded fallback responsive without waking the shell at frame-like cadence;
// unchanged snapshots are filtered before any GTK updates occur.
const RAIL_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const BACKGROUND_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const SHELL_CSS_BASE: &str = include_str!("shell.css");

thread_local! {
    /// SVG parsing, font discovery, and rasterization are intentionally shared
    /// by every shell window on the GTK thread. Dynamic rail updates can select
    /// an icon frequently, but a given icon/size pair only needs rendering once.
    static SHELL_ICON_CACHE: RefCell<HashMap<(String, u32), gdk::Texture>> =
        RefCell::new(HashMap::new());
    // Display cards are rebuilt from the compositor snapshot, but the person
    // using the companion should not lose an open adjustment section every
    // time that live state refreshes.
    static COMPANION_EXPANDED_DISPLAYS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRole {
    Panel,
    Dock,
    Companion,
}

impl ShellRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Panel => "focaldesk-system-rail",
            Self::Dock => "focaldesk-task-shelf",
            Self::Companion => "focaldesk-companion-dashboard",
        }
    }

    const fn application_id(self) -> &'static str {
        match self {
            Self::Panel => "dev.focaldesk.SystemRail",
            Self::Dock => "dev.focaldesk.TaskShelf",
            Self::Companion => "dev.focaldesk.CompanionDashboard",
        }
    }

    const fn layer_default_size(self) -> (i32, i32) {
        match self {
            Self::Panel => (SYSTEM_RAIL_WIDTH, 0),
            Self::Dock => (TASK_SHELF_WIDTH, TASK_SHELF_HEIGHT),
            Self::Companion => (0, 0),
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
    if role == ShellRole::Companion {
        // Remain alive while a portable panel is unplugged so monitor hotplug
        // can create its dashboard without a service restart loop.
        std::mem::forget(app.hold());
    }
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
    let normal_connectors = normal_connectors();
    let mut output_count = 0usize;
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else {
            continue;
        };
        let connector = monitor
            .connector()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("output-{}", index + 1));
        let is_companion_output = companion_connectors().contains(&connector);
        // A ControlPanel is exclusively owned by the full-screen companion.
        // Do not put the normal desktop rail or task shelf underneath it: in
        // addition to redundant chrome, those overlapping layer surfaces can
        // compete for the panel's touch focus.
        if (role == ShellRole::Companion && !is_companion_output)
            || (role != ShellRole::Companion && is_companion_output)
            // GTK can briefly enumerate compositor aliases such as
            // `output-4` alongside the physical DP connector. Never create a
            // second rail/dock surface on those aliases.
            || (role != ShellRole::Companion && !normal_connectors.contains(&connector))
        {
            continue;
        }
        let window = match role {
            ShellRole::Dock => build_dock(app, config, connector.clone()),
            ShellRole::Panel => build_panel(app, index + 1, config, connector.clone()),
            ShellRole::Companion => build_companion(app, connector.clone()),
        };
        let vui_client = window
            .child()
            .is_some_and(|child| child.is::<gtk::DrawingArea>());
        configure_layer_window(&window, role, &monitor, config);
        if vui_client {
            report_shell_frame_state(role, connector.clone(), monitors.n_items() as usize, false);
        }
        window.present();
        if !vui_client {
            report_shell_frame_state(role, connector, monitors.n_items() as usize, true);
        }
        output_count += 1;
    }

    if output_count == 0 && role != ShellRole::Companion {
        let connector = "output-1".to_string();
        let window = match role {
            ShellRole::Dock => build_dock(app, config, connector.clone()),
            ShellRole::Panel => build_panel(app, 1, config, connector.clone()),
            ShellRole::Companion => unreachable!("companion has no fallback output"),
        };
        let vui_client = window
            .child()
            .is_some_and(|child| child.is::<gtk::DrawingArea>());
        configure_layer_window_without_monitor(&window, role, config);
        if vui_client {
            report_shell_frame_state(role, connector.clone(), 1, false);
        }
        window.present();
        if !vui_client {
            report_shell_frame_state(role, connector, 1, true);
        }
        output_count = 1;
    }

    flog(format!(
        "{}: presented {output_count} GTK layer surface(s)",
        role.namespace()
    ));
}

fn report_shell_frame_state(role: ShellRole, connector: String, output_count: usize, ready: bool) {
    if let Err(error) = send_desktop_request(&IpcRequest::ShellReady {
        namespace: role.namespace().to_string(),
        output_count,
        connector: Some(connector.clone()),
        ready,
    }) {
        focaldesk_logging::flog_warn!(
            "{} could not report frame readiness for {connector}: {error}",
            role.namespace()
        );
    }
}

fn configure_layer_window(
    window: &gtk::ApplicationWindow,
    role: ShellRole,
    monitor: &gdk::Monitor,
    config: &FocalDeskConfig,
) {
    configure_layer_window_without_monitor(window, role, config);
    if role == ShellRole::Panel
        && window
            .child()
            .is_some_and(|child| child.is::<gtk::DrawingArea>())
    {
        // A VUI rail is a single DrawingArea with a one-pixel natural height.
        // Do not rely on layer-shell inferring a stretched size from that
        // child; give the anchored surface its actual per-monitor height.
        let geometry = monitor.geometry();
        let height = (geometry.height() - 24).max(1);
        window.set_default_size(SYSTEM_RAIL_WIDTH, height);
    }
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
        ShellRole::Companion => {
            window.add_css_class("companion-dashboard-window");
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Bottom, true);
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
            window.set_default_size(1280, 720);
            window.set_exclusive_zone(-1);
        }
    }
}

fn companion_connectors() -> Vec<String> {
    load_settings()
        .displays
        .outputs
        .into_iter()
        .filter(|output| {
            output.enabled && output.role == focaldesk_settings_core::OutputRole::ControlPanel
        })
        .map(|output| output.connector)
        .collect()
}

fn normal_connectors() -> Vec<String> {
    load_settings()
        .displays
        .outputs
        .into_iter()
        .filter(|output| {
            output.enabled && output.role == focaldesk_settings_core::OutputRole::Normal
        })
        .map(|output| output.connector)
        .collect()
}

fn companion_workspace_target() -> Option<String> {
    load_settings()
        .displays
        .outputs
        .into_iter()
        .find(|output| output.enabled && output.primary)
        .map(|output| output.connector)
}

fn open_settings_on_focused_normal_output() {
    thread::spawn(|| {
        let normal = normal_connectors();
        let target = send_desktop_request(&IpcRequest::GetDesktopSnapshot)
            .ok()
            .and_then(|response| match response {
                IpcResponse::DesktopSnapshot { snapshot } => snapshot
                    .outputs
                    .into_iter()
                    .find(|output| output.focused && normal.contains(&output.connector))
                    .map(|output| (output.connector, output.active_workspace_id)),
                _ => None,
            })
            .or_else(|| companion_workspace_target().map(|connector| (connector, 1)));

        if let Some((connector, workspace)) = target {
            let _ = send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                action: DesktopAction::FocusWorkspaceOnOutput {
                    connector,
                    workspace: workspace.max(1),
                },
            });
        }
        if let Err(error) = Command::new("focaldesk-settings").spawn() {
            flog(format!("open Settings from Companion failed: {error}"));
        }
    });
}

fn open_display_settings_on_focused_normal_output() {
    thread::spawn(|| {
        let normal = normal_connectors();
        let target = send_desktop_request(&IpcRequest::GetDesktopSnapshot)
            .ok()
            .and_then(|response| match response {
                IpcResponse::DesktopSnapshot { snapshot } => snapshot
                    .outputs
                    .into_iter()
                    .find(|output| output.focused && normal.contains(&output.connector))
                    .map(|output| (output.connector, output.active_workspace_id)),
                _ => None,
            })
            .or_else(|| companion_workspace_target().map(|connector| (connector, 1)));

        if let Some((connector, workspace)) = target {
            let _ = send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                action: DesktopAction::FocusWorkspaceOnOutput {
                    connector,
                    workspace: workspace.max(1),
                },
            });
        }
        // Settings already supports a bounded `--panel` argument. Launching
        // it here preserves that argument while the compositor has just
        // focused the intended normal output.
        if let Err(error) = Command::new("focaldesk-settings")
            .args(["--panel", "Displays"])
            .spawn()
        {
            flog(format!(
                "open Display Settings from Companion failed: {error}"
            ));
        }
    });
}

fn open_network_settings_on_focused_normal_output() {
    thread::spawn(|| {
        let normal = normal_connectors();
        let target = send_desktop_request(&IpcRequest::GetDesktopSnapshot)
            .ok()
            .and_then(|response| match response {
                IpcResponse::DesktopSnapshot { snapshot } => snapshot
                    .outputs
                    .into_iter()
                    .find(|output| output.focused && normal.contains(&output.connector))
                    .map(|output| (output.connector, output.active_workspace_id)),
                _ => None,
            })
            .or_else(|| companion_workspace_target().map(|connector| (connector, 1)));
        if let Some((connector, workspace)) = target {
            let _ = send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                action: DesktopAction::FocusWorkspaceOnOutput {
                    connector,
                    workspace: workspace.max(1),
                },
            });
        }
        if let Err(error) = Command::new("focaldesk-settings")
            .args(["--panel", "Network"])
            .spawn()
        {
            flog(format!(
                "open Network Settings from Companion failed: {error}"
            ));
        }
    });
}

fn build_companion(app: &gtk::Application, _connector: String) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("FocalDesk Companion")
        .build();
    // The Control Panel toolbar is an overlay rather than a normal flow child:
    // page content may grow or switch, but the touch controls must remain
    // visible at the bottom of the display.
    let root = gtk::Overlay::new();
    root.add_css_class("companion-dashboard");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_child(Some(&content));

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    header.add_css_class("companion-header");
    let brand = gtk::Label::new(Some("FocalDesk"));
    brand.add_css_class("companion-brand");
    brand.set_hexpand(true);
    brand.set_xalign(0.0);
    header.append(&brand);
    let assistant_nav = Rc::new(RefCell::new(None::<gtk::Button>));
    let home_nav = Rc::new(RefCell::new(None::<gtk::Button>));
    let workspaces_nav = Rc::new(RefCell::new(None::<gtk::Button>));
    let system_nav = Rc::new(RefCell::new(None::<gtk::Button>));
    let companion_nav_buttons = Rc::new(RefCell::new(Vec::<gtk::Button>::new()));
    for label in ["⌂  Home", "♩  Assistant", "▦  Workspaces", "⚙  System"] {
        let button = gtk::Button::with_label(label);
        button.add_css_class("companion-nav");
        if label.contains("Assistant") {
            *assistant_nav.borrow_mut() = Some(button.clone());
        } else if label.contains("Home") {
            *home_nav.borrow_mut() = Some(button.clone());
        } else if label.contains("Workspaces") {
            *workspaces_nav.borrow_mut() = Some(button.clone());
        } else if label.contains("System") {
            *system_nav.borrow_mut() = Some(button.clone());
        }
        companion_nav_buttons.borrow_mut().push(button.clone());
        header.append(&button);
    }
    if let Some(home) = home_nav.borrow().as_ref() {
        set_companion_nav_selected(&companion_nav_buttons, home);
    }
    let clock = gtk::Label::new(None);
    clock.add_css_class("companion-header-clock");
    header.append(&clock);
    content.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    body.add_css_class("companion-body");
    let left = gtk::Box::new(gtk::Orientation::Vertical, 14);
    left.set_size_request(250, -1);
    let date = companion_card("Date & Time");
    let big_time = gtk::Label::new(None);
    big_time.add_css_class("companion-big-time");
    date.append(&big_time);
    let date_label = companion_label("", "companion-subtitle");
    date.append(&date_label);
    left.append(&date);
    let weather = companion_card("Weather");
    let weather_summary = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let weather_icon = companion_label("◌", "companion-weather-icon");
    weather_icon.set_yalign(0.5);
    weather_summary.append(&weather_icon);
    let weather_reading = gtk::Box::new(gtk::Orientation::Vertical, 2);
    weather_reading.set_valign(gtk::Align::Center);
    let weather_condition =
        companion_label("Loading local weather…", "companion-weather-condition");
    let weather_temperature = companion_label("", "companion-weather-temperature");
    weather_reading.append(&weather_condition);
    weather_reading.append(&weather_temperature);
    weather_summary.append(&weather_reading);
    weather.append(&weather_summary);
    let weather_details = companion_label("Humidity —   Wind —", "companion-weather-details");
    weather.append(&weather_details);
    left.append(&weather);
    let media = companion_card("Media");
    let media_art = gtk::Picture::new();
    media_art.set_size_request(-1, 82);
    media_art.set_can_shrink(true);
    media_art.add_css_class("companion-album");
    media.append(&media_art);
    let media_title = companion_label("Nothing playing", "companion-media-text");
    let media_artist = companion_label(
        "Open a compatible media player to begin.",
        "companion-subtitle",
    );
    let media_position = companion_label("", "companion-media-controls");
    media.append(&media_title);
    media.append(&media_artist);
    media.append(&media_position);
    let media_service = Rc::new(RefCell::new(None::<String>));
    let media_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    media_controls.set_halign(gtk::Align::Center);
    for (label, method) in [("◀", "Previous"), ("▶", "PlayPause"), ("▶|", "Next")] {
        let button = gtk::Button::with_label(label);
        button.add_css_class("companion-outline-button");
        let service = media_service.clone();
        button.connect_clicked(move |_| {
            if let Some(service) = service.borrow().as_deref() {
                let _ = media_player_command(service, method);
            }
        });
        media_controls.append(&button);
    }
    media.append(&media_controls);
    left.append(&media);
    body.append(&left);

    let center = companion_card("Conversation");
    center.set_hexpand(true);
    center.set_vexpand(true);
    let listening = companion_label("●  Listening", "companion-listening");
    listening.set_halign(gtk::Align::End);
    center.append(&listening);
    let wave = gtk::Label::new(Some("·▁▂▄▆▄▂▅▇▃▅█▆▃▅▇█▆▃▆▇▄▂▅▇▄▂▃▅▃▂·"));
    wave.add_css_class("companion-waveform");
    wave.set_vexpand(true);
    center.append(&wave);
    center.append(&companion_label(
        "Live transcription",
        "companion-section-title",
    ));
    let transcript = companion_field("No active transcription.");
    center.append(&transcript);
    center.append(&companion_label("AI response", "companion-section-title"));
    let response = companion_field("No AI response received in this session.");
    center.append(&response);
    let compose = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let message = gtk::Entry::new();
    message.set_placeholder_text(Some("Type a message…"));
    message.set_width_chars(28);
    message.set_hexpand(true);
    message.add_css_class("companion-field");
    let send = gtk::Button::with_label("➤  Send");
    send.add_css_class("companion-outline-button");
    let message_for_send = message.clone();
    let response_for_send = response.clone();
    send.connect_clicked(move |_| {
        let prompt = message_for_send.text().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        message_for_send.set_text("");
        response_for_send.set_text("Generating response…");
        thread::spawn(move || {
            if let Err(error) = send_ai_request(&AiIpcRequest::Chat {
                request: ChatRequest::from_prompt(prompt),
            }) {
                flog(format!("Companion AI message failed: {error}"));
            }
        });
    });
    compose.append(&message);
    compose.append(&send);
    let use_transcription = gtk::Button::with_label("Use transcription");
    use_transcription.add_css_class("companion-outline-button");
    use_transcription.set_sensitive(false);
    use_transcription.set_tooltip_text(Some(
        "Copy the latest completed transcription into the message box.",
    ));
    body.append(&center);

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 14);
    controls.set_size_request(250, -1);
    let quick = companion_card("Quick controls");
    for (name, value) in [
        ("🔊  Volume", pipewire_volume_percent().unwrap_or(70.0)),
        (
            "☼  Brightness",
            viewsonic_brightness_percent().unwrap_or(100.0),
        ),
    ] {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&companion_label(name, "companion-control-label"));
        let value_text = format!("{value:.0}%");
        let value_label = companion_label(&value_text, "companion-value");
        value_label.set_hexpand(true);
        value_label.set_halign(gtk::Align::End);
        row.append(&value_label);
        quick.append(&row);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_value(value);
        scale.set_draw_value(false);
        if name.contains("Volume") {
            let value_label = value_label.clone();
            scale.connect_value_changed(move |scale| {
                let value = scale.value().round().clamp(0.0, 100.0);
                value_label.set_text(&format!("{value:.0}%"));
                if let Err(error) = set_pipewire_volume(value) {
                    flog(format!("Companion volume update failed: {error}"));
                }
            });
        } else {
            let value_label = value_label.clone();
            scale.set_tooltip_text(Some("Controls the ViewSonic TD1655 hardware backlight."));
            scale.connect_value_changed(move |scale| {
                let value = scale.value().round().clamp(0.0, 100.0);
                value_label.set_text(&format!("{value:.0}%"));
                thread::spawn(move || {
                    if let Err(error) = set_viewsonic_brightness(value) {
                        flog(format!("Companion brightness update failed: {error}"));
                    }
                });
            });
        }
        quick.append(&scale);
    }
    let audio = gtk::Button::with_label("🔊  Audio output                                      ›");
    audio.add_css_class("companion-outline-button");
    audio.connect_clicked(|_| open_settings_on_focused_normal_output());
    quick.append(&audio);
    controls.append(&quick);
    let workspaces = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let workspace_card = companion_card("Workspaces");
    let workspace_buttons = Rc::new(RefCell::new(Vec::<(u32, gtk::Button)>::new()));
    let workspace_count = desktop_snapshot()
        .map(|snapshot| snapshot.shell.workspace_count)
        .filter(|count| *count > 0)
        .unwrap_or(1)
        .min(64) as u32;
    for workspace in 1..=workspace_count {
        let button = gtk::Button::with_label(&format!(
            "{workspace} — {}",
            configured_workspace_name(workspace)
        ));
        button.add_css_class("companion-workspace");
        let target = companion_workspace_target();
        let workspace_buttons_for_click = workspace_buttons.clone();
        button.connect_clicked(move |_| {
            // The companion has no reason to make a successful press
            // invisible. Latch the chosen workspace immediately; the next
            // dashboard state refresh may replace it with authoritative state.
            for (candidate, candidate_button) in workspace_buttons_for_click.borrow().iter() {
                if *candidate == workspace {
                    candidate_button.add_css_class("suggested-action");
                } else {
                    candidate_button.remove_css_class("suggested-action");
                }
            }
            let action = target
                .clone()
                .map(|connector| DesktopAction::FocusWorkspaceOnOutput {
                    connector,
                    workspace,
                })
                .unwrap_or(DesktopAction::FocusWorkspace { workspace });
            send_action(action);
        });
        workspace_buttons
            .borrow_mut()
            .push((workspace, button.clone()));
        workspaces.attach(
            &button,
            ((workspace - 1) % 2) as i32,
            ((workspace - 1) / 2) as i32,
            1,
            1,
        );
    }
    workspace_card.append(&workspaces);
    controls.append(&workspace_card);
    body.append(&controls);
    let workspaces_body = build_workspaces_tab();
    let system_body = build_system_tab();
    let pages = gtk::Stack::new();
    pages.set_hexpand(true);
    pages.set_vexpand(true);
    pages.add_named(&body, Some("home"));
    pages.add_named(&workspaces_body, Some("workspaces"));
    pages.add_named(&system_body, Some("system"));
    pages.set_visible_child_name("home");

    if let Some(button) = assistant_nav.borrow().as_ref() {
        let left = left.clone();
        let controls = controls.clone();
        let pages = pages.clone();
        let nav_buttons = companion_nav_buttons.clone();
        button.connect_clicked(move |button| {
            pages.set_visible_child_name("home");
            left.set_visible(false);
            controls.set_visible(false);
            set_companion_nav_selected(&nav_buttons, button);
        });
    }
    if let Some(button) = home_nav.borrow().as_ref() {
        let left = left.clone();
        let controls = controls.clone();
        let pages = pages.clone();
        let nav_buttons = companion_nav_buttons.clone();
        button.connect_clicked(move |button| {
            pages.set_visible_child_name("home");
            left.set_visible(true);
            controls.set_visible(true);
            set_companion_nav_selected(&nav_buttons, button);
        });
    }
    if let Some(button) = workspaces_nav.borrow().as_ref() {
        let pages = pages.clone();
        let nav_buttons = companion_nav_buttons.clone();
        button.connect_clicked(move |button| {
            pages.set_visible_child_name("workspaces");
            set_companion_nav_selected(&nav_buttons, button);
        });
    }
    if let Some(button) = system_nav.borrow().as_ref() {
        let pages = pages.clone();
        let nav_buttons = companion_nav_buttons.clone();
        button.connect_clicked(move |button| {
            pages.set_visible_child_name("system");
            set_companion_nav_selected(&nav_buttons, button);
        });
    }
    // The conversation can be taller than the Control Panel display. Keep it
    // in a scrollable viewport so it cannot push the fixed bottom toolbar
    // beyond the layer surface.
    let page_viewport = gtk::ScrolledWindow::new();
    page_viewport.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    page_viewport.set_hexpand(true);
    page_viewport.set_vexpand(true);
    page_viewport.set_margin_bottom(104);
    page_viewport.set_child(Some(&pages));
    content.append(&page_viewport);

    let dictation = Rc::new(RefCell::new(CompanionDictation::default()));
    let dictation_for_transcript = dictation.clone();
    let message_for_transcript = message.clone();
    use_transcription.connect_clicked(move |_| {
        if let Some(text) = dictation_for_transcript.borrow().final_text.clone() {
            message_for_transcript.set_text(&text);
            message_for_transcript.grab_focus();
        }
    });
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 22);
    footer.add_css_class("companion-footer");
    footer.set_hexpand(true);
    footer.set_halign(gtk::Align::Fill);
    footer.set_valign(gtk::Align::End);
    let wake_word = gtk::Label::new(Some(&companion_wake_word_label()));
    wake_word.add_css_class("companion-footer-label");
    footer.append(&wake_word);
    footer.append(&companion_label(
        "🎙  Mic active ●",
        "companion-footer-label",
    ));
    let read_replies = gtk::CheckButton::with_label("Read replies aloud");
    read_replies.set_tooltip_text(Some(
        "Speak new AI replies through the configured speech service.",
    ));
    let spoken_response = Rc::new(RefCell::new(None::<String>));
    read_replies.connect_toggled(move |toggle| {
        if !toggle.is_active() {
            let _ = send_speech_request(&SpeechIpcRequest::stop());
        }
    });
    footer.append(&read_replies);
    // Keep the composer in the fixed toolbar: long AI replies must never
    // bury the action that sends typed or transcribed text.
    footer.append(&compose);
    footer.append(&use_transcription);
    let mic = gtk::Button::with_label("🎙\nTalk / Stop");
    mic.add_css_class("companion-mic");
    let dictation_for_mic = dictation.clone();
    let transcript_for_mic = transcript.clone();
    let use_transcription_for_mic = use_transcription.clone();
    mic.connect_clicked(move |_| {
        let mut dictation = dictation_for_mic.borrow_mut();
        if let Some(lease_id) = dictation.lease_id.clone() {
            match send_microphone_request(&MicrophoneIpcRequest::Stop {
                lease_id: Some(lease_id),
            }) {
                Ok(_) => transcript_for_mic.set_text("Finishing dictation…"),
                Err(error) => {
                    transcript_for_mic.set_text(&format!("Could not stop dictation: {error}"))
                }
            }
        } else {
            dictation.final_text = None;
            use_transcription_for_mic.set_sensitive(false);
            match send_microphone_request(&MicrophoneIpcRequest::Start {
                requester: "focaldesk-companion-dashboard".into(),
            }) {
                Ok(state) if state.status != "error" => {
                    dictation.lease_id = state.lease_id;
                    dictation.after_sequence = state.latest_sequence;
                    transcript_for_mic.set_text("Starting microphone…");
                }
                Ok(state) => transcript_for_mic.set_text(
                    state
                        .message
                        .as_deref()
                        .unwrap_or("Microphone request was rejected."),
                ),
                Err(error) => {
                    transcript_for_mic.set_text(&format!("Could not start dictation: {error}"))
                }
            }
        }
    });
    footer.append(&mic);
    footer.append(&companion_label("ⓧ  Cancel", "companion-footer-label"));
    let voice_preferences = gtk::Button::with_label("🎙  Voice preferences");
    let wake_word_for_preferences = wake_word.clone();
    let window_for_voice_preferences = window.clone();
    voice_preferences.connect_clicked(move |_| {
        open_companion_voice_preferences(&window_for_voice_preferences, &wake_word_for_preferences)
    });
    footer.append(&voice_preferences);
    let settings = gtk::Button::with_label("⚙  Settings");
    settings.connect_clicked(|_| open_settings_on_focused_normal_output());
    footer.append(&settings);
    root.add_overlay(&footer);

    refresh_companion_clock(&clock, &big_time, &date_label);
    glib::timeout_add_local(Duration::from_secs(1), move || {
        refresh_companion_clock(&clock, &big_time, &date_label);
        glib::ControlFlow::Continue
    });
    let weather_state = start_companion_weather_updates();
    refresh_companion_weather(
        &weather_icon,
        &weather_condition,
        &weather_temperature,
        &weather_details,
        &weather_state,
    );
    glib::timeout_add_local(Duration::from_secs(5), move || {
        refresh_companion_weather(
            &weather_icon,
            &weather_condition,
            &weather_temperature,
            &weather_details,
            &weather_state,
        );
        glib::ControlFlow::Continue
    });
    refresh_companion_ai_activity(&listening, &wave);
    glib::timeout_add_local(Duration::from_millis(500), move || {
        refresh_companion_ai_activity(&listening, &wave);
        glib::ControlFlow::Continue
    });
    refresh_companion_conversation(
        &transcript,
        &response,
        &dictation,
        &message,
        &use_transcription,
        &read_replies,
        &spoken_response,
    );
    glib::timeout_add_local(Duration::from_millis(250), move || {
        refresh_companion_conversation(
            &transcript,
            &response,
            &dictation,
            &message,
            &use_transcription,
            &read_replies,
            &spoken_response,
        );
        glib::ControlFlow::Continue
    });
    refresh_companion_media(
        &media_title,
        &media_artist,
        &media_position,
        &media_art,
        &media_service,
    );
    glib::timeout_add_local(Duration::from_secs(1), move || {
        refresh_companion_media(
            &media_title,
            &media_artist,
            &media_position,
            &media_art,
            &media_service,
        );
        glib::ControlFlow::Continue
    });

    window.set_child(Some(&root));
    window
}

fn set_companion_nav_selected(buttons: &Rc<RefCell<Vec<gtk::Button>>>, selected: &gtk::Button) {
    for button in buttons.borrow().iter() {
        button.remove_css_class("suggested-action");
    }
    selected.add_css_class("suggested-action");
}

fn build_workspaces_tab() -> gtk::ScrolledWindow {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.add_css_class("companion-body");
    let title = gtk::Label::new(Some("Workspaces"));
    title.add_css_class("companion-workspaces-title");
    title.set_halign(gtk::Align::Start);
    body.append(&title);
    let subtitle = companion_label(
        "Tap a workspace to switch it on that display.",
        "companion-subtitle",
    );
    subtitle.set_halign(gtk::Align::Start);
    body.append(&subtitle);
    let displays = gtk::Grid::builder()
        .row_spacing(14)
        .column_spacing(14)
        .hexpand(true)
        .vexpand(true)
        .build();
    body.append(&displays);

    let dashboard_signature = Rc::new(RefCell::new(None::<String>));
    refresh_workspace_dashboard(&displays, &dashboard_signature);
    let dashboard_signature_for_refresh = dashboard_signature.clone();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        refresh_workspace_dashboard(&displays, &dashboard_signature_for_refresh);
        glib::ControlFlow::Continue
    });

    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class("companion-system-scroll");
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));
    scroll
}

fn refresh_workspace_dashboard(
    container: &gtk::Grid,
    last_signature: &Rc<RefCell<Option<String>>>,
) {
    let Some(snapshot) = desktop_snapshot() else {
        return;
    };
    let settings = load_settings();
    let normal = settings
        .displays
        .outputs
        .iter()
        .filter(|output| output.enabled && output.role == OutputRole::Normal)
        .map(|output| output.connector.clone())
        .collect::<Vec<_>>();
    let workspace_count = snapshot.shell.workspace_count.max(1);
    let maximum = snapshot.shell.max_workspace_slots.max(1);
    let signature = format!(
        "{normal:?}|{workspace_count}|{maximum}|{:?}|{}|{}",
        settings.workspaces.workspace_names,
        snapshot
            .outputs
            .iter()
            .filter(|output| normal.contains(&output.connector))
            .map(|output| format!(
                "{}:{}:{}:{}:{}",
                output.id, output.connector, output.active_workspace_id, output.make, output.model
            ))
            .collect::<Vec<_>>()
            .join("|"),
        snapshot
            .windows
            .iter()
            .filter(|window| window.mapped)
            .map(|window| format!(
                "{}:{:?}:{}",
                window.id, window.output_id, window.workspace_id
            ))
            .collect::<Vec<_>>()
            .join("|")
    );
    if last_signature.borrow().as_deref() == Some(signature.as_str()) {
        return;
    }
    *last_signature.borrow_mut() = Some(signature);
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    // The compositor restores a session snapshot before applying configured
    // overrides. An absent override must stay "Workspace N", rather than
    // inheriting stale session metadata such as a wallpaper path.
    let configured_workspace_names = settings.workspaces.workspace_names;
    let workspace_names = snapshot
        .workspaces
        .iter()
        .map(|workspace| {
            let name = configured_workspace_names
                .get(&workspace.id)
                .map(|name| name.trim())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Workspace {}", workspace.id));
            (workspace.id, name)
        })
        .collect::<HashMap<_, _>>();
    let outputs = snapshot
        .outputs
        .iter()
        .filter(|output| normal.contains(&output.connector))
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        container.attach(
            &companion_field("No normal desktop displays are available."),
            0,
            0,
            1,
            1,
        );
        return;
    }
    for (index, output) in outputs.into_iter().enumerate() {
        let card = companion_card(&format!("▣  {}", output.connector));
        card.add_css_class("companion-workspace-output");
        card.set_hexpand(true);
        let model = [output.make.as_str(), output.model.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let active_workspace_name = workspace_names
            .get(&output.active_workspace_id)
            .cloned()
            .unwrap_or_else(|| format!("Workspace {}", output.active_workspace_id));
        let summary = if model.is_empty() {
            format!("Connected · {active_workspace_name} active")
        } else {
            format!("Connected · {model} · {active_workspace_name} active")
        };
        card.append(&companion_label(&summary, "companion-workspace-summary"));
        let workspaces = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(8)
            .hexpand(true)
            .build();
        for workspace in 1..=workspace_count as u32 {
            let active = output.active_workspace_id == workspace;
            let window_count = snapshot
                .windows
                .iter()
                .filter(|window| {
                    window.mapped
                        && window.output_id == Some(output.id)
                        && window.workspace_id == workspace
                })
                .count();
            let workspace_name = workspace_names
                .get(&workspace)
                .cloned()
                .unwrap_or_else(|| format!("Workspace {workspace}"));
            let label = if active {
                format!("{workspace} · {workspace_name}\nActive · {window_count} windows")
            } else {
                format!("{workspace} · {workspace_name}\n{window_count} windows")
            };
            let button = gtk::Button::with_label(&label);
            button.add_css_class("companion-workspace");
            button.add_css_class("companion-workspace-tile");
            button.set_hexpand(true);
            if active {
                button.add_css_class("suggested-action");
            }
            let connector = output.connector.clone();
            button.connect_clicked(move |_| {
                send_action(DesktopAction::FocusWorkspaceOnOutput {
                    connector: connector.clone(),
                    workspace,
                });
            });
            workspaces.attach(
                &button,
                ((workspace - 1) % 2) as i32,
                ((workspace - 1) / 2) as i32,
                1,
                1,
            );
        }
        card.append(&workspaces);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add = gtk::Button::with_label("＋  New workspace");
        add.add_css_class("companion-outline-button");
        add.set_hexpand(true);
        add.set_sensitive(workspace_count < maximum);
        let connector = output.connector.clone();
        add.connect_clicked(move |_| {
            send_action(DesktopAction::CreateWorkspaceOnOutput {
                connector: connector.clone(),
            });
        });
        actions.append(&add);
        let remove = gtk::Button::with_label("−  Delete current");
        remove.add_css_class("companion-outline-button");
        remove.set_hexpand(true);
        remove.set_sensitive(workspace_count > 1);
        let connector = output.connector.clone();
        remove.connect_clicked(move |_| {
            send_action(DesktopAction::DeleteWorkspaceOnOutput {
                connector: connector.clone(),
            });
        });
        actions.append(&remove);
        let rename = gtk::Button::with_label("✎  Rename");
        rename.add_css_class("companion-outline-button");
        rename.set_hexpand(true);
        let workspace = output.active_workspace_id;
        let workspace_name = active_workspace_name.clone();
        rename.connect_clicked(move |_| rename_workspace_dialog(workspace, &workspace_name));
        actions.append(&rename);
        card.append(&actions);
        let move_focused = gtk::Button::with_label("⇄  Move focused window");
        move_focused.add_css_class("companion-outline-button");
        move_focused.set_hexpand(true);
        move_focused.connect_clicked(move |button| show_move_focused_window_picker(button));
        card.append(&move_focused);
        container.attach(&card, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
}

fn rename_workspace_dialog(workspace: u32, current_name: &str) {
    let dialog = gtk::Dialog::builder()
        .modal(true)
        .title(format!("Rename workspace {workspace}"))
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Save name", gtk::ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(8);
    content.append(&companion_label(
        "Workspace names are shared by all desktop displays.",
        "companion-control-label",
    ));
    let entry = gtk::Entry::new();
    entry.set_text(current_name);
    entry.set_activates_default(true);
    entry.set_hexpand(true);
    entry.add_css_class("companion-field");
    content.append(&entry);
    dialog.set_default_response(gtk::ResponseType::Accept);
    dialog.connect_response(move |dialog, response| {
        let name = entry.text().trim().to_string();
        dialog.close();
        if response != gtk::ResponseType::Accept {
            return;
        }
        thread::spawn(move || {
            if let Err(error) = set_workspace_name(workspace, &name) {
                flog(format!("Could not rename workspace {workspace}: {error}"));
            }
        });
    });
    dialog.show();
}

fn set_workspace_name(workspace: u32, name: &str) -> Result<(), String> {
    let name = name.trim();
    if workspace == 0 {
        return Err("workspace must be positive".into());
    }
    if name.is_empty() {
        return Err("workspace name cannot be empty".into());
    }
    let name = name.chars().take(64).collect::<String>();
    let mut settings = load_settings();
    settings.workspaces.workspace_names.insert(workspace, name);
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::Reload) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(response) => Err(format!("unexpected compositor response: {response:?}")),
        Err(error) => Err(format!("reload workspace settings: {error}")),
    }
}

fn configured_workspace_name(workspace: u32) -> String {
    load_settings()
        .workspaces
        .workspace_names
        .get(&workspace)
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Workspace {workspace}"))
}

fn show_move_focused_window_picker(anchor: &gtk::Button) {
    let popover = gtk::Popover::new();
    popover.add_css_class("companion-workspace-picker");
    popover.set_parent(anchor);
    popover.set_position(gtk::PositionType::Top);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.add_css_class("companion-workspace-picker-body");
    body.set_margin_top(10);
    body.set_margin_bottom(10);
    body.set_margin_start(10);
    body.set_margin_end(10);
    let Some(snapshot) = desktop_snapshot() else {
        body.append(&companion_label(
            "Workspace state is unavailable.",
            "companion-control-label",
        ));
        popover.set_child(Some(&body));
        popover.popup();
        return;
    };
    let Some(window_id) = snapshot.session.focused_window_id else {
        body.append(&companion_label(
            "No desktop window is focused.",
            "companion-control-label",
        ));
        body.append(&companion_label(
            "Select an application window, then return here.",
            "companion-subtitle",
        ));
        popover.set_child(Some(&body));
        popover.popup();
        return;
    };
    let title = snapshot
        .windows
        .iter()
        .find(|window| window.id == window_id)
        .map(|window| window.title.as_str())
        .filter(|title| !title.is_empty())
        .unwrap_or("focused window");
    let current_workspace = snapshot
        .windows
        .iter()
        .find(|window| window.id == window_id)
        .map(|window| window.workspace_id);
    let source_connector = snapshot
        .windows
        .iter()
        .find(|window| window.id == window_id)
        .and_then(|window| window.output_id)
        .and_then(|output_id| {
            snapshot
                .outputs
                .iter()
                .find(|output| output.id == output_id)
                .map(|output| output.connector.clone())
        });
    // The shell's count is authoritative.  Do not use the descriptive workspace
    // vector as the list of move targets: during a topology/session refresh it
    // can briefly be stale even though the numbered workspace slots are valid.
    let workspace_count = snapshot
        .shell
        .workspace_count
        .max(snapshot.workspaces.len())
        .min(64);
    let destinations = (1..=workspace_count as u32)
        .filter(|workspace| Some(*workspace) != current_workspace)
        .collect::<Vec<_>>();
    body.append(&companion_label(
        &format!("Move {title} to:"),
        "companion-control-label",
    ));
    if destinations.is_empty() {
        body.append(&companion_label(
            "There is no other workspace.",
            "companion-subtitle",
        ));
    }
    let choices = gtk::Box::new(gtk::Orientation::Vertical, 6);
    choices.set_hexpand(true);
    for workspace in destinations {
        let name = configured_workspace_name(workspace);
        let button = gtk::Button::with_label(&format!("{workspace} — {name}"));
        button.add_css_class("companion-outline-button");
        button.set_hexpand(true);
        button.set_width_request(320);
        button.set_height_request(42);
        let popover_for_click = popover.clone();
        let source_connector = source_connector.clone();
        button.connect_clicked(move |_| {
            popover_for_click.popdown();
            move_focused_window_and_follow(window_id, workspace, source_connector.clone());
        });
        choices.append(&button);
    }
    body.append(&choices);
    popover.set_child(Some(&body));
    popover.popup();
}

fn move_focused_window_and_follow(window_id: u32, workspace: u32, connector: Option<String>) {
    thread::spawn(move || {
        let request = IpcRequest::ExecuteDesktopAction {
            action: DesktopAction::MoveWindowToWorkspace {
                window_id,
                workspace,
            },
        };
        match send_desktop_request(&request) {
            Ok(IpcResponse::Ok) => {
                if let Some(connector) = connector {
                    match send_desktop_request(&IpcRequest::ExecuteDesktopAction {
                        action: DesktopAction::FocusWorkspaceOnOutput {
                            connector,
                            workspace,
                        },
                    }) {
                        Ok(IpcResponse::Ok) => {}
                        Ok(IpcResponse::Error { message }) => {
                            flog(format!("Moved window but could not show it: {message}"));
                        }
                        Ok(response) => {
                            flog(format!(
                                "Moved window but could not show it: unexpected response {response:?}"
                            ));
                        }
                        Err(error) => flog(format!("Moved window but could not show it: {error}")),
                    }
                }
            }
            Ok(IpcResponse::Error { message }) => {
                flog(format!("Could not move focused window: {message}"));
            }
            Ok(response) => flog(format!("Could not move focused window: {response:?}")),
            Err(error) => flog(format!("Could not move focused window: {error}")),
        }
    });
}

fn build_system_tab() -> gtk::ScrolledWindow {
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    body.add_css_class("companion-body");
    let left = gtk::Box::new(gtk::Orientation::Vertical, 14);
    let audio = companion_card("🔊  Audio");
    let audio_status = companion_field("");
    audio.append(&audio_status);
    let output_picker = pipewire_device_picker("Output device", "Sinks");
    audio.append(&output_picker);
    let input_picker = pipewire_device_picker("Microphone", "Sources");
    audio.append(&input_picker);
    let audio_settings = gtk::Button::with_label("Audio output  ›");
    audio_settings.add_css_class("companion-outline-button");
    audio_settings.connect_clicked(|_| open_settings_on_focused_normal_output());
    audio.append(&audio_settings);
    left.append(&audio);
    let connections = companion_card("⌁  Connections");
    let network_status = companion_field("");
    connections.append(&network_status);
    let wifi_networks = gtk::Box::new(gtk::Orientation::Vertical, 6);
    connections.append(&wifi_networks);
    let refresh_wifi = gtk::Button::with_label("Refresh Wi‑Fi networks");
    refresh_wifi.add_css_class("companion-outline-button");
    refresh_wifi.connect_clicked(|_| {
        thread::spawn(|| {
            let _ = Command::new("nmcli")
                .args(["device", "wifi", "rescan"])
                .status();
        });
    });
    connections.append(&refresh_wifi);
    let bluetooth_devices = gtk::Box::new(gtk::Orientation::Vertical, 6);
    connections.append(&bluetooth_devices);
    let scan_bluetooth = gtk::Button::with_label("Scan for Bluetooth devices");
    scan_bluetooth.add_css_class("companion-outline-button");
    scan_bluetooth.connect_clicked(|_| {
        thread::spawn(move || {
            let _ = Command::new("bluetoothctl")
                .args(["--timeout", "8", "scan", "on"])
                .status();
        });
    });
    connections.append(&scan_bluetooth);
    left.append(&connections);
    body.append(&left);

    let right = gtk::Box::new(gtk::Orientation::Vertical, 14);
    right.set_hexpand(true);
    let displays = companion_card("▣  Displays");
    let display_cards = gtk::Box::new(gtk::Orientation::Vertical, 8);
    displays.append(&display_cards);
    let display_settings = gtk::Button::with_label("Display settings  ›");
    display_settings.add_css_class("companion-outline-button");
    display_settings.connect_clicked(|_| open_display_settings_on_focused_normal_output());
    displays.append(&display_settings);
    right.append(&displays);
    let performance = companion_card("◴  Performance");
    let performance_status = companion_field("");
    performance.append(&performance_status);
    right.append(&performance);
    let power = companion_card("⏻  Power");
    power.append(&companion_power_button(
        "Lock",
        CompanionPowerAction::Lock,
        false,
    ));
    power.append(&companion_power_button(
        "Suspend",
        CompanionPowerAction::Suspend,
        false,
    ));
    power.append(&companion_power_button(
        "Restart",
        CompanionPowerAction::Restart,
        true,
    ));
    power.append(&companion_power_button(
        "Power off",
        CompanionPowerAction::PowerOff,
        true,
    ));
    right.append(&power);
    body.append(&right);

    refresh_system_tab(
        &audio_status,
        &network_status,
        &wifi_networks,
        &bluetooth_devices,
        &display_cards,
        &performance_status,
    );
    glib::timeout_add_local(Duration::from_secs(2), move || {
        refresh_system_tab(
            &audio_status,
            &network_status,
            &wifi_networks,
            &bluetooth_devices,
            &display_cards,
            &performance_status,
        );
        glib::ControlFlow::Continue
    });
    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class("companion-system-scroll");
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));
    scroll
}

#[derive(Clone, Copy)]
enum CompanionPowerAction {
    Lock,
    Suspend,
    Restart,
    PowerOff,
}

impl CompanionPowerAction {
    fn label(self) -> &'static str {
        match self {
            Self::Lock => "Lock",
            Self::Suspend => "Suspend",
            Self::Restart => "Restart",
            Self::PowerOff => "Power off",
        }
    }

    fn confirmation(self) -> &'static str {
        match self {
            Self::Restart => "Restart the computer now? Open work will be closed by applications that cannot restore it.",
            Self::PowerOff => "Power off the computer now? Open work will be closed by applications that cannot restore it.",
            Self::Lock | Self::Suspend => "",
        }
    }
}

fn companion_power_button(
    label: &str,
    action: CompanionPowerAction,
    needs_confirmation: bool,
) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("companion-outline-button");
    button.connect_clicked(move |_| {
        if !needs_confirmation {
            thread::spawn(move || run_companion_power_action(action));
            return;
        }
        let dialog = gtk::MessageDialog::builder()
            .modal(true)
            .text(action.confirmation())
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button(action.label(), gtk::ResponseType::Accept);
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Accept {
                thread::spawn(move || run_companion_power_action(action));
            }
        });
        dialog.show();
    });
    button
}

fn run_companion_power_action(action: CompanionPowerAction) {
    let result = match action {
        CompanionPowerAction::Lock => Command::new("loginctl")
            .arg("lock-session")
            .status()
            .map_err(|error| error.to_string())
            .and_then(|status| {
                status
                    .success()
                    .then_some(())
                    .ok_or_else(|| status.to_string())
            }),
        CompanionPowerAction::Suspend => send_companion_power_request(PowerIpcRequest::Suspend),
        CompanionPowerAction::Restart => send_companion_power_request(PowerIpcRequest::Reboot),
        CompanionPowerAction::PowerOff => send_companion_power_request(PowerIpcRequest::PowerOff),
    };
    if let Err(error) = result {
        flog(format!(
            "Companion {} action failed: {error}",
            action.label()
        ));
    }
}

fn send_companion_power_request(request: PowerIpcRequest) -> Result<(), String> {
    match send_power_request(&request) {
        Ok(PowerIpcResponse::Ok) => Ok(()),
        Ok(PowerIpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected power response: {other:?}")),
        Err(error) => Err(error),
    }
}

fn refresh_system_tab(
    audio: &gtk::Label,
    network: &gtk::Label,
    wifi_networks: &gtk::Box,
    bluetooth_devices: &gtk::Box,
    displays: &gtk::Box,
    performance: &gtk::Label,
) {
    audio.set_text(&format!(
        "Default output volume: {:.0}%\nChoose the PipeWire output and microphone below.",
        pipewire_volume_percent().unwrap_or(0.0)
    ));
    let wifi = Command::new("nmcli")
        .args(["-t", "-f", "ACTIVE,SSID", "device", "wifi"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("yes:"))
                .map(str::to_owned)
        })
        .and_then(|line| line.split_once(':').map(|(_, ssid)| ssid.to_string()))
        .filter(|ssid| !ssid.is_empty())
        .unwrap_or_else(|| "Disconnected".into());
    let bluetooth = bluetooth_state();
    network.set_text(&format!("Wi‑Fi: {wifi}\n{}", bluetooth_summary(&bluetooth)));
    refresh_wifi_networks(wifi_networks);
    refresh_bluetooth_devices(bluetooth_devices, &bluetooth);
    refresh_display_cards(displays);
    performance.set_text(&performance_summary());
}

fn performance_summary() -> String {
    let cpu_load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_string))
        .map(|load| format!("CPU load: {load}"))
        .unwrap_or_else(|| "CPU load: unavailable".into());
    let cpu_temp = Command::new("sensors")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| {
            text.lines()
                .find(|line| {
                    line.contains("Package id 0:") || line.trim_start().starts_with("Tctl:")
                })
                .and_then(|line| line.split('+').nth(1))
                .and_then(|value| value.split('°').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("CPU temperature: {value}°C"))
        })
        .unwrap_or_else(|| "CPU temperature: unavailable".into());
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            let total = text
                .lines()
                .find(|line| line.starts_with("MemTotal:"))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()?;
            let available = text
                .lines()
                .find(|line| line.starts_with("MemAvailable:"))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()?;
            Some((total - available, total))
        })
        .map(|(used, total)| {
            format!(
                "Memory: {:.1} / {:.1} GB",
                used as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            )
        })
        .unwrap_or_else(|| "Memory: unavailable".into());
    let storage = Command::new("df")
        .args(["-h", "/"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.lines().nth(1).map(str::to_string))
        .and_then(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() >= 5).then(|| {
                format!(
                    "Storage: {} free of {} ({})",
                    fields[3], fields[1], fields[4]
                )
            })
        })
        .unwrap_or_else(|| "Storage: unavailable".into());
    let gpu = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            let fields = text.trim().split(',').map(str::trim).collect::<Vec<_>>();
            (fields.len() == 4).then(|| {
                format!(
                    "GPU: {}% · {}°C · VRAM {} / {} MiB",
                    fields[0], fields[1], fields[2], fields[3]
                )
            })
        })
        .unwrap_or_else(|| "GPU: unavailable".into());
    format!("{cpu_load}\n{cpu_temp}\n{mem}\n{storage}\n{gpu}")
}

#[derive(Clone, Debug)]
struct BluetoothDevice {
    address: String,
    name: String,
    connected: bool,
}

#[derive(Clone, Debug, Default)]
struct BluetoothState {
    powered: bool,
    devices: Vec<BluetoothDevice>,
}

fn bluetooth_state() -> BluetoothState {
    let controller = Command::new("bluetoothctl")
        .arg("show")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    let powered = controller
        .as_deref()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("Powered:"))
        })
        .map(|value| value.trim().eq_ignore_ascii_case("yes"))
        .unwrap_or(false);
    if !powered {
        return BluetoothState::default();
    }
    let devices = Command::new("bluetoothctl")
        .args(["devices", "Paired"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| line.strip_prefix("Device "))
                .filter_map(|line| {
                    let (address, name) = line.split_once(' ')?;
                    let connected = Command::new("bluetoothctl")
                        .args(["info", address])
                        .output()
                        .ok()
                        .filter(|output| output.status.success())
                        .and_then(|output| String::from_utf8(output.stdout).ok())
                        .is_some_and(|info| {
                            info.lines().any(|line| line.trim() == "Connected: yes")
                        });
                    Some(BluetoothDevice {
                        address: address.to_string(),
                        name: name.trim().to_string(),
                        connected,
                    })
                })
                .filter(|device| !device.name.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    BluetoothState { powered, devices }
}

fn bluetooth_summary(state: &BluetoothState) -> String {
    if !state.powered {
        return "Bluetooth: Off or unavailable".into();
    }
    let connected = state
        .devices
        .iter()
        .filter(|device| device.connected)
        .map(|device| device.name.as_str())
        .collect::<Vec<_>>();
    if connected.is_empty() {
        "Bluetooth: On · no connected devices".into()
    } else {
        format!("Bluetooth: On · {}", connected.join(", "))
    }
}

fn refresh_bluetooth_devices(container: &gtk::Box, state: &BluetoothState) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    if !state.powered {
        container.append(&companion_label(
            "Turn Bluetooth on in Settings to manage devices.",
            "companion-control-label",
        ));
        return;
    }
    if state.devices.is_empty() {
        container.append(&companion_label(
            "No paired Bluetooth devices.",
            "companion-control-label",
        ));
        return;
    }
    for device in &state.devices {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = companion_label(
            &format!(
                "{} · {}",
                device.name,
                if device.connected {
                    "Connected"
                } else {
                    "Paired"
                }
            ),
            "companion-control-label",
        );
        name.set_hexpand(true);
        row.append(&name);
        let button = gtk::Button::with_label(if device.connected {
            "Disconnect"
        } else {
            "Connect"
        });
        button.add_css_class("companion-outline-button");
        let address = device.address.clone();
        let command = if device.connected {
            "disconnect"
        } else {
            "connect"
        };
        button.connect_clicked(move |button| {
            button.set_sensitive(false);
            button.set_label(if command == "connect" {
                "Connecting…"
            } else {
                "Disconnecting…"
            });
            let address = address.clone();
            thread::spawn(move || {
                match Command::new("bluetoothctl")
                    .args([command, &address])
                    .status()
                {
                    Ok(status) if status.success() => {}
                    Ok(status) => flog(format!(
                        "bluetoothctl {command} {address} exited with {status}"
                    )),
                    Err(error) => flog(format!("bluetoothctl {command} {address} failed: {error}")),
                }
            });
        });
        row.append(&button);
        container.append(&row);
    }
}

#[derive(Clone, Debug)]
struct WifiNetwork {
    ssid: String,
    signal: String,
    active: bool,
    saved: bool,
}

fn refresh_wifi_networks(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let saved = Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE", "connection", "show"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| line.rsplit_once(':'))
                .filter(|(_, kind)| *kind == "802-11-wireless")
                .map(|(name, _)| name.to_string())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let networks = Command::new("nmcli")
        .args(["-t", "-f", "IN-USE,SSID,SIGNAL", "device", "wifi", "list"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| {
                    let mut fields = line.splitn(3, ':');
                    let in_use = fields.next()?;
                    let ssid = fields.next()?.trim();
                    let signal = fields.next()?.trim();
                    (!ssid.is_empty()).then(|| WifiNetwork {
                        ssid: ssid.to_string(),
                        signal: signal.to_string(),
                        active: in_use == "*",
                        saved: saved.contains(ssid),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if networks.is_empty() {
        container.append(&companion_label(
            "No Wi‑Fi networks found.",
            "companion-control-label",
        ));
        return;
    }
    for network in networks.into_iter().take(8) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let state = if network.active {
            "Connected"
        } else if network.saved {
            "Saved"
        } else {
            "Password required"
        };
        let label = companion_label(
            &format!("{} · {}% · {state}", network.ssid, network.signal),
            "companion-control-label",
        );
        label.set_hexpand(true);
        row.append(&label);
        if network.active {
            let button = gtk::Button::with_label("Connected");
            button.add_css_class("companion-outline-button");
            button.set_sensitive(false);
            row.append(&button);
        } else if network.saved {
            let button = gtk::Button::with_label("Connect");
            button.add_css_class("companion-outline-button");
            let ssid = network.ssid.clone();
            button.connect_clicked(move |button| {
                button.set_sensitive(false);
                button.set_label("Connecting…");
                let ssid = ssid.clone();
                thread::spawn(move || {
                    match Command::new("nmcli")
                        .args(["connection", "up", "id", &ssid])
                        .status()
                    {
                        Ok(status) if status.success() => {}
                        Ok(status) => flog(format!("nmcli connect {ssid} exited with {status}")),
                        Err(error) => flog(format!("nmcli connect {ssid} failed: {error}")),
                    }
                });
            });
            row.append(&button);
        } else {
            let button = gtk::Button::with_label("Network settings");
            button.add_css_class("companion-outline-button");
            button.connect_clicked(|_| open_network_settings_on_focused_normal_output());
            row.append(&button);
        }
        container.append(&row);
    }
}

fn refresh_display_cards(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let settings = load_settings();
    let ddc_connectors = ddc_capable_connectors();
    let Some(snapshot) = desktop_snapshot() else {
        container.append(&companion_field("Display state unavailable."));
        return;
    };
    if snapshot.outputs.is_empty() {
        container.append(&companion_field("No active displays detected."));
        return;
    }
    for output in snapshot.outputs {
        let configured = settings
            .displays
            .outputs
            .iter()
            .find(|configured| configured.connector == output.connector);
        let role = match configured.map(|configured| configured.role) {
            Some(OutputRole::ControlPanel) => "Control panel",
            _ => "Desktop",
        };
        let primary = configured.is_some_and(|configured| configured.primary);
        let refresh_hz = output.refresh_mhz as f64 / 1000.0;
        let manufacturer = [output.make.as_str(), output.model.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let title = if manufacturer.is_empty() {
            output.connector.clone()
        } else {
            format!("{} · {}", output.connector, manufacturer)
        };
        let flags = [
            primary.then_some("Primary"),
            output.focused.then_some("Focused"),
            Some(role),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let ddc_brightness = ddc_connectors
            .contains(&output.connector)
            .then(|| cached_ddc_brightness_percent(&output.connector))
            .flatten();
        let brightness = match ddc_brightness {
            Some(value) => format!("DDC/CI brightness: {value:.0}%"),
            None if ddc_connectors.contains(&output.connector) => {
                "DDC/CI brightness: available".into()
            }
            None => "DDC/CI brightness: unavailable".into(),
        };
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("companion-card");
        card.append(&companion_field(&format!(
            "{title}\n{}×{} · {:.1} Hz · {:.0}% scale\n{flags}\n{brightness}",
            output.width,
            output.height,
            refresh_hz,
            output.scale * 100.0,
        )));
        card.append(&hdr_display_control(&output));
        if ddc_connectors.contains(&output.connector) {
            card.append(&ddc_brightness_control(&output.connector, ddc_brightness));
        }
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);
        controls.append(&orientation_picker(&output.connector, output.transform));
        controls.append(&scale_picker(&output.connector, output.scale));
        controls.append(&refresh_picker(
            &output.connector,
            output.width,
            output.height,
            output.refresh_mhz,
        ));
        controls.append(&resolution_picker(
            &output.connector,
            output.width,
            output.height,
            output.refresh_mhz,
        ));
        let connector = output.connector.clone();
        let expanded =
            COMPANION_EXPANDED_DISPLAYS.with(|expanded| expanded.borrow().contains(&connector));
        controls.set_visible(expanded);
        let adjust = gtk::Button::with_label(if expanded {
            "Hide display controls"
        } else {
            "Adjust display"
        });
        adjust.add_css_class("companion-outline-button");
        let controls_for_toggle = controls.clone();
        adjust.connect_clicked(move |button| {
            let expanded = COMPANION_EXPANDED_DISPLAYS.with(|expanded| {
                let mut expanded = expanded.borrow_mut();
                if expanded.remove(&connector) {
                    false
                } else {
                    expanded.insert(connector.clone());
                    true
                }
            });
            controls_for_toggle.set_visible(expanded);
            button.set_label(if expanded {
                "Hide display controls"
            } else {
                "Adjust display"
            });
        });
        card.append(&adjust);
        card.append(&controls);
        if role == "Desktop" {
            let button = gtk::Button::with_label(if primary {
                "●  Primary display"
            } else {
                "Set as primary"
            });
            button.add_css_class("companion-outline-button");
            button.set_sensitive(!primary);
            let connector = output.connector.clone();
            button.connect_clicked(move |button| {
                button.set_sensitive(false);
                button.set_label("Setting primary display…");
                let connector = connector.clone();
                thread::spawn(move || {
                    if let Err(error) = set_primary_display(&connector) {
                        flog(format!(
                            "Could not set primary display {connector}: {error}"
                        ));
                    }
                });
            });
            card.append(&button);

            let control_panel = gtk::Button::with_label("Use as control panel");
            control_panel.add_css_class("companion-outline-button");
            let connector = output.connector.clone();
            control_panel.connect_clicked(move |button| {
                let dialog = gtk::MessageDialog::builder()
                    .modal(true)
                    .text("Use this display as the FocalDesk control panel?")
                    .secondary_text(
                        "The companion dashboard will move here. The current control-panel display will return to the normal desktop layout.",
                    )
                    .build();
                dialog.add_button("Cancel", gtk::ResponseType::Cancel);
                dialog.add_button("Use as control panel", gtk::ResponseType::Accept);
                let connector = connector.clone();
                let button = button.clone();
                dialog.connect_response(move |dialog, response| {
                    dialog.close();
                    if response != gtk::ResponseType::Accept {
                        return;
                    }
                    button.set_sensitive(false);
                    button.set_label("Moving control panel…");
                    let connector = connector.clone();
                    thread::spawn(move || {
                        if let Err(error) = set_control_panel_display(&connector) {
                            flog(format!(
                                "Could not make {connector} the control panel: {error}"
                            ));
                        }
                    });
                });
                dialog.show();
            });
            card.append(&control_panel);
        }
        container.append(&card);
    }
}

fn hdr_display_control(output: &OutputSnapshot) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Fill);
    let status = if output.hdr_active {
        if output.wide_gamut_active {
            "HDR: Active · wide gamut"
        } else {
            "HDR: Active"
        }
    } else if output.hdr_requested {
        "HDR: Requested, but inactive"
    } else if output.hdr_supported {
        "HDR: Available"
    } else {
        "HDR: Not supported"
    };
    let detail = if output.icc_lut_fallback_active {
        format!("{status} · ICC fallback")
    } else {
        status.to_string()
    };
    let label = companion_label(&detail, "companion-control-label");
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    row.append(&label);
    if output.hdr_supported {
        let requested = output.hdr_requested || output.hdr_active;
        let button = gtk::Button::with_label(if requested {
            "Turn HDR off"
        } else {
            "Turn HDR on"
        });
        button.add_css_class("companion-outline-button");
        let connector = output.connector.clone();
        button.connect_clicked(move |button| {
            let requested = button.label().is_some_and(|label| label == "Turn HDR on");
            let action = if requested {
                "Turn HDR on"
            } else {
                "Turn HDR off"
            };
            let dialog = gtk::MessageDialog::builder()
                .modal(true)
                .text(format!("{action} for {connector}?"))
                .secondary_text(
                    "The display may briefly blank while FocalDesk changes its output mode.",
                )
                .build();
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button(action, gtk::ResponseType::Accept);
            let connector = connector.clone();
            let button = button.clone();
            dialog.connect_response(move |dialog, response| {
                dialog.close();
                if response != gtk::ResponseType::Accept {
                    return;
                }
                button.set_sensitive(false);
                button.set_label("Applying HDR…");
                let connector = connector.clone();
                thread::spawn(move || {
                    if let Err(error) = set_display_hdr_requested(&connector, requested) {
                        flog(format!(
                            "Could not update HDR request for {connector}: {error}"
                        ));
                    }
                });
            });
            dialog.show();
        });
        row.append(&button);
    }
    row
}

fn orientation_picker(connector: &str, current: DisplayTransform) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label("Orientation", "companion-control-label"));
    let picker = gtk::ComboBoxText::new();
    picker.add_css_class("companion-outline-button");
    for (id, label) in [
        ("normal", "Landscape"),
        ("rotate90", "Portrait (clockwise)"),
        ("rotate180", "Landscape (upside down)"),
        ("rotate270", "Portrait (counter-clockwise)"),
    ] {
        picker.append(Some(id), label);
    }
    picker.set_active_id(Some(display_transform_id(current)));
    let connector = connector.to_string();
    picker.connect_changed(move |picker| {
        let Some(id) = picker.active_id() else {
            return;
        };
        let Some(transform) = display_transform_from_id(id.as_str()) else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .modal(true)
            .text("Change display orientation?")
            .secondary_text(
                "The display will rotate immediately. Choose Cancel if you selected it by accident.",
            )
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Apply orientation", gtk::ResponseType::Accept);
        let connector = connector.clone();
        let picker = picker.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Accept {
                // The periodic live-card refresh restores the authoritative
                // selection shortly after a cancelled request.
                return;
            }
            picker.set_sensitive(false);
            let connector = connector.clone();
            thread::spawn(move || {
                if let Err(error) = set_display_transform(&connector, transform) {
                    flog(format!("Could not rotate display {connector}: {error}"));
                }
            });
        });
        dialog.show();
    });
    row.append(&picker);
    row
}

fn display_transform_id(transform: DisplayTransform) -> &'static str {
    match transform {
        DisplayTransform::Normal => "normal",
        DisplayTransform::Rotate90 => "rotate90",
        DisplayTransform::Rotate180 => "rotate180",
        DisplayTransform::Rotate270 => "rotate270",
    }
}

fn display_transform_from_id(id: &str) -> Option<DisplayTransform> {
    match id {
        "normal" => Some(DisplayTransform::Normal),
        "rotate90" => Some(DisplayTransform::Rotate90),
        "rotate180" => Some(DisplayTransform::Rotate180),
        "rotate270" => Some(DisplayTransform::Rotate270),
        _ => None,
    }
}

fn scale_picker(connector: &str, current: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label("Scale", "companion-control-label"));
    let picker = gtk::ComboBoxText::new();
    picker.add_css_class("companion-outline-button");
    let values = [1.0_f64, 1.25, 1.5, 1.75, 2.0];
    for value in values {
        picker.append(
            Some(&format!("{value:.2}")),
            &format!("{:.0}%", value * 100.0),
        );
    }
    let current_id = format!("{current:.2}");
    if values.iter().any(|value| (*value - current).abs() < 0.01) {
        picker.set_active_id(Some(&current_id));
    } else {
        picker.append(
            Some(&current_id),
            &format!("{:.0}% (current)", current * 100.0),
        );
        picker.set_active_id(Some(&current_id));
    }
    let connector = connector.to_string();
    picker.connect_changed(move |picker| {
        let Some(id) = picker.active_id() else {
            return;
        };
        let Ok(scale) = id.parse::<f32>() else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .modal(true)
            .text("Change display scale?")
            .secondary_text("Applications and the desktop layout will reflow immediately.")
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Apply scale", gtk::ResponseType::Accept);
        let connector = connector.clone();
        let picker = picker.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Accept {
                return;
            }
            picker.set_sensitive(false);
            let connector = connector.clone();
            thread::spawn(move || {
                if let Err(error) = set_display_scale(&connector, scale) {
                    flog(format!("Could not scale display {connector}: {error}"));
                }
            });
        });
        dialog.show();
    });
    row.append(&picker);
    row
}

fn set_display_hdr_requested(connector: &str, requested: bool) -> Result<(), String> {
    let mut settings = load_settings();
    let Some(output) = settings
        .displays
        .outputs
        .iter_mut()
        .find(|output| output.connector == connector && output.enabled)
    else {
        return Err("selected output is not enabled".into());
    };
    output.hdr_requested = requested;
    if !requested {
        output.hdr_enabled = false;
    }
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: settings.displays.outputs,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(response) => Err(format!("unexpected compositor response: {response:?}")),
        Err(error) => Err(format!("apply display settings: {error}")),
    }
}

fn set_display_transform(connector: &str, transform: DisplayTransform) -> Result<(), String> {
    let mut settings = load_settings();
    let Some(output) = settings
        .displays
        .outputs
        .iter_mut()
        .find(|output| output.enabled && output.connector == connector)
    else {
        return Err("selected output is not enabled".into());
    };
    output.transform = transform;
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: settings.displays.outputs,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected display response: {other:?}")),
        Err(error) => Err(error),
    }
}

fn set_display_scale(connector: &str, scale: f32) -> Result<(), String> {
    if !scale.is_finite() || !(1.0..=4.0).contains(&scale) {
        return Err("scale must be between 100% and 400%".into());
    }
    let mut settings = load_settings();
    let Some(output) = settings
        .displays
        .outputs
        .iter_mut()
        .find(|output| output.enabled && output.connector == connector)
    else {
        return Err("selected output is not enabled".into());
    };
    output.scale = scale;
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: settings.displays.outputs,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected display response: {other:?}")),
        Err(error) => Err(error),
    }
}

fn refresh_picker(connector: &str, width: i32, height: i32, current: i32) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label("Refresh rate", "companion-control-label"));
    let picker = gtk::ComboBoxText::new();
    picker.add_css_class("companion-outline-button");
    let rates = available_refresh_rates(connector, width, height, current);
    for rate in &rates {
        picker.append(
            Some(&rate.to_string()),
            &format!("{:.2} Hz", *rate as f64 / 1000.0),
        );
    }
    picker.set_active_id(Some(&current.to_string()));
    let connector = connector.to_string();
    picker.connect_changed(move |picker| {
        let Some(id) = picker.active_id() else {
            return;
        };
        let Ok(refresh_mhz) = id.parse::<i32>() else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .modal(true)
            .text("Change display refresh rate?")
            .secondary_text("The display may go blank briefly while the new rate is applied.")
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Apply refresh rate", gtk::ResponseType::Accept);
        let connector = connector.clone();
        let picker = picker.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Accept {
                return;
            }
            picker.set_sensitive(false);
            let connector = connector.clone();
            thread::spawn(move || {
                if let Err(error) = set_display_refresh(&connector, refresh_mhz) {
                    flog(format!(
                        "Could not set refresh rate for {connector}: {error}"
                    ));
                }
            });
        });
        dialog.show();
    });
    row.append(&picker);
    row
}

/// Reads the compositor-generated discovery inventory but never modifies it.
/// The typed settings document remains the only source we write to.
fn available_refresh_rates(connector: &str, width: i32, height: i32, current: i32) -> Vec<i32> {
    let mut rates = available_display_modes(connector)
        .into_iter()
        .filter(|(mode_width, mode_height, _)| *mode_width == width && *mode_height == height)
        .map(|(_, _, rate)| rate)
        .collect::<Vec<_>>();
    rates.push(current);
    rates.sort_unstable();
    rates.dedup();
    rates
}

/// Reads the compositor-generated discovery inventory but never modifies it.
/// The typed settings document remains the only source we write to.
fn available_display_modes(connector: &str) -> Vec<(i32, i32, i32)> {
    let path = focaldesk_settings_core::settings_path()
        .parent()
        .map(|path| path.join("displays.json"));
    path.and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|inventory| inventory.as_array().cloned())
        .and_then(|displays| {
            displays.into_iter().find(|display| {
                display.get("name").and_then(serde_json::Value::as_str) == Some(connector)
            })
        })
        .and_then(|display| {
            display
                .get("available_modes")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .map(|modes| {
            modes
                .into_iter()
                .filter_map(|mode| {
                    Some((
                        i32::try_from(mode.get("width")?.as_i64()?).ok()?,
                        i32::try_from(mode.get("height")?.as_i64()?).ok()?,
                        i32::try_from(mode.get("refresh_mhz")?.as_i64()?).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resolution_picker(connector: &str, width: i32, height: i32, current_refresh: i32) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label("Resolution", "companion-control-label"));
    let picker = gtk::ComboBoxText::new();
    picker.add_css_class("companion-outline-button");
    let mut resolutions = available_display_modes(connector)
        .into_iter()
        .map(|(width, height, _)| (width, height))
        .collect::<Vec<_>>();
    resolutions.push((width, height));
    resolutions.sort_unstable_by(|left, right| (right.0 * right.1).cmp(&(left.0 * left.1)));
    resolutions.dedup();
    for (mode_width, mode_height) in &resolutions {
        picker.append(
            Some(&format!("{mode_width}x{mode_height}")),
            &format!("{mode_width} × {mode_height}"),
        );
    }
    picker.set_active_id(Some(&format!("{width}x{height}")));
    let connector = connector.to_string();
    picker.connect_changed(move |picker| {
        let Some(id) = picker.active_id() else {
            return;
        };
        let Some((width, height)) = id.split_once('x').and_then(|(width, height)| {
            Some((width.parse::<i32>().ok()?, height.parse::<i32>().ok()?))
        }) else {
            return;
        };
        let rates = available_refresh_rates(&connector, width, height, current_refresh);
        let refresh_mhz = rates
            .iter()
            .find(|rate| **rate == current_refresh)
            .copied()
            .or_else(|| rates.last().copied())
            .unwrap_or(current_refresh);
        let dialog = gtk::MessageDialog::builder()
            .modal(true)
            .text("Change display resolution?")
            .secondary_text(&format!(
                "This applies {width} × {height} at {:.2} Hz. The display may go blank briefly.",
                refresh_mhz as f64 / 1000.0
            ))
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Apply resolution", gtk::ResponseType::Accept);
        let connector = connector.clone();
        let picker = picker.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Accept {
                return;
            }
            picker.set_sensitive(false);
            let connector = connector.clone();
            thread::spawn(move || {
                if let Err(error) = set_display_mode(&connector, width, height, refresh_mhz) {
                    flog(format!("Could not set resolution for {connector}: {error}"));
                }
            });
        });
        dialog.show();
    });
    row.append(&picker);
    row
}

fn set_display_refresh(connector: &str, refresh_mhz: i32) -> Result<(), String> {
    if refresh_mhz <= 0 {
        return Err("refresh rate must be positive".into());
    }
    let mut settings = load_settings();
    let Some(output) = settings
        .displays
        .outputs
        .iter_mut()
        .find(|output| output.enabled && output.connector == connector)
    else {
        return Err("selected output is not enabled".into());
    };
    output.refresh_mhz = refresh_mhz;
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: settings.displays.outputs,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected display response: {other:?}")),
        Err(error) => Err(error),
    }
}

fn set_display_mode(
    connector: &str,
    width: i32,
    height: i32,
    refresh_mhz: i32,
) -> Result<(), String> {
    if width <= 0 || height <= 0 || refresh_mhz <= 0 {
        return Err("display mode must have a positive size and refresh rate".into());
    }
    let mut settings = load_settings();
    let Some(output) = settings
        .displays
        .outputs
        .iter_mut()
        .find(|output| output.enabled && output.connector == connector)
    else {
        return Err("selected output is not enabled".into());
    };
    output.width = width;
    output.height = height;
    output.refresh_mhz = refresh_mhz;
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    match send_desktop_request(&IpcRequest::SetDisplays {
        outputs: settings.displays.outputs,
    }) {
        Ok(IpcResponse::Ok) => Ok(()),
        Ok(IpcResponse::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected display response: {other:?}")),
        Err(error) => Err(error),
    }
}

fn set_primary_display(connector: &str) -> Result<(), String> {
    let mut settings = load_settings();
    let target_exists = settings.displays.outputs.iter().any(|output| {
        output.enabled && output.role == OutputRole::Normal && output.connector == connector
    });
    if !target_exists {
        return Err("selected output is not an enabled desktop display".into());
    }
    for output in &mut settings.displays.outputs {
        if output.enabled && output.role == OutputRole::Normal {
            output.primary = output.connector == connector;
        }
    }
    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    send_desktop_request(&IpcRequest::Reload)
        .map_err(|error| format!("reload compositor settings: {error}"))?;
    Ok(())
}

fn set_control_panel_display(connector: &str) -> Result<(), String> {
    let mut settings = load_settings();
    let target_exists = settings
        .displays
        .outputs
        .iter()
        .any(|output| output.enabled && output.connector == connector);
    if !target_exists {
        return Err("selected output is not enabled".into());
    }
    let has_desktop_after_switch = settings
        .displays
        .outputs
        .iter()
        .any(|output| output.enabled && output.connector != connector);
    if !has_desktop_after_switch {
        return Err("a control panel requires at least one other enabled desktop display".into());
    }

    for output in &mut settings.displays.outputs {
        if !output.enabled {
            continue;
        }
        output.role = if output.connector == connector {
            OutputRole::ControlPanel
        } else {
            OutputRole::Normal
        };
        if output.connector == connector {
            output.primary = false;
        }
    }
    if !settings
        .displays
        .outputs
        .iter()
        .any(|output| output.enabled && output.role == OutputRole::Normal && output.primary)
    {
        if let Some(fallback) = settings
            .displays
            .outputs
            .iter_mut()
            .find(|output| output.enabled && output.role == OutputRole::Normal)
        {
            fallback.primary = true;
        }
    }

    save_settings(&settings).map_err(|error| format!("save settings: {error}"))?;
    send_desktop_request(&IpcRequest::Reload)
        .map_err(|error| format!("reload compositor settings: {error}"))?;
    let status = Command::new("systemctl")
        .args(["--user", "restart", "focaldesk-companion-dashboard.service"])
        .status()
        .map_err(|error| format!("restart companion dashboard: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("restart companion dashboard exited with {status}"))
    }
}

fn ddc_capable_connectors() -> &'static HashSet<String> {
    static CONNECTORS: OnceLock<HashSet<String>> = OnceLock::new();
    CONNECTORS.get_or_init(|| {
        let output = Command::new("sudo")
            .args([
                "-n",
                "/usr/local/libexec/focaldesk-viewsonic-brightness",
                "detect",
            ])
            .output();
        let Ok(output) = output else {
            return HashSet::new();
        };
        if !output.status.success() {
            return HashSet::new();
        }
        let mut connectors = HashSet::new();
        for connector in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().strip_prefix("DRM_connector:"))
            .map(|connector| connector.trim().to_string())
        {
            if let Some((_, short_name)) = connector
                .strip_prefix("card")
                .and_then(|name| name.split_once('-'))
            {
                connectors.insert(short_name.to_string());
            }
            connectors.insert(connector);
        }
        connectors
    })
}

fn is_viewsonic_td1655(output: &OutputSnapshot) -> bool {
    output.make.eq_ignore_ascii_case("VSC")
        || output.make.to_ascii_lowercase().contains("viewsonic")
        || output.model.eq_ignore_ascii_case("TD1655")
}

fn ddc_brightness_cache() -> &'static Mutex<HashMap<String, Option<f64>>> {
    static BRIGHTNESS: OnceLock<Mutex<HashMap<String, Option<f64>>>> = OnceLock::new();
    BRIGHTNESS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_ddc_brightness_percent(connector: &str) -> Option<f64> {
    let mut brightness = ddc_brightness_cache().lock().ok()?;
    if let Some(value) = brightness.get(connector) {
        return *value;
    }
    let value = ddc_brightness_percent(connector);
    brightness.insert(connector.to_string(), value);
    value
}

fn remember_ddc_brightness(connector: &str, value: f64) {
    if let Ok(mut brightness) = ddc_brightness_cache().lock() {
        brightness.insert(connector.to_string(), Some(value));
    }
}

fn ddc_brightness_control(connector: &str, current: Option<f64>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label(
        "Hardware brightness",
        "companion-control-label",
    ));
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_hexpand(true);
    scale.set_draw_value(false);
    let current = current.unwrap_or(50.0);
    scale.set_value(current);
    let value_label = companion_label(&format!("{current:.0}%"), "companion-control-value");
    let connector = connector.to_string();
    let value_label_for_change = value_label.clone();
    scale.connect_value_changed(move |scale| {
        let value = scale.value().round().clamp(0.0, 100.0);
        value_label_for_change.set_text(&format!("{value:.0}%"));
        let connector = connector.clone();
        thread::spawn(move || match set_ddc_brightness(&connector, value) {
            Ok(()) => remember_ddc_brightness(&connector, value),
            Err(error) => flog(format!(
                "Companion brightness update for {connector} failed: {error}"
            )),
        });
    });
    controls.append(&scale);
    controls.append(&value_label);
    row.append(&controls);
    row
}

/// A live PipeWire endpoint as reported by `wpctl status`.
#[derive(Clone, Debug)]
struct PipewireNode {
    id: String,
    name: String,
    active: bool,
}

fn pipewire_device_picker(label: &str, section: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&companion_label(label, "companion-control-label"));

    let picker = gtk::ComboBoxText::new();
    picker.add_css_class("companion-outline-button");
    let nodes = pipewire_nodes(section);
    if nodes.is_empty() {
        picker.append(Some(""), "No devices available");
        picker.set_sensitive(false);
    } else {
        for node in &nodes {
            picker.append(Some(&node.id), &node.name);
        }
        if let Some(active) = nodes.iter().find(|node| node.active) {
            picker.set_active_id(Some(&active.id));
        } else {
            picker.set_active(Some(0));
        }
    }
    picker.connect_changed(move |picker| {
        let Some(id) = picker.active_id() else {
            return;
        };
        if id.is_empty() {
            return;
        }
        let id = id.to_string();
        thread::spawn(move || {
            let result = Command::new("wpctl").args(["set-default", &id]).status();
            match result {
                Ok(status) if status.success() => {}
                Ok(status) => flog(format!("PipeWire set-default {id} exited with {status}")),
                Err(error) => flog(format!("Could not select PipeWire device {id}: {error}")),
            }
        });
    });
    row.append(&picker);
    row
}

fn pipewire_nodes(section: &str) -> Vec<PipewireNode> {
    let Ok(output) = Command::new("wpctl").arg("status").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    let mut in_section = false;
    let mut nodes = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == format!("├─ {section}:") || trimmed == format!("└─ {section}:") {
            in_section = true;
            continue;
        }
        if in_section && (trimmed.starts_with("├─") || trimmed.starts_with("└─")) {
            break;
        }
        if !in_section {
            continue;
        }
        // `wpctl status` prefixes endpoints with a tree glyph (`│`) and may
        // place the active marker after it, e.g. `│  *  109. Speakers`.
        let candidate = trimmed.trim_start_matches(['│', ' ', '*']).trim();
        let active = trimmed.contains('*');
        let Some((id, rest)) = candidate.split_once('.') else {
            continue;
        };
        if id.parse::<u32>().is_err() {
            continue;
        }
        let name = rest.split("  [").next().unwrap_or(rest).trim().to_string();
        if !name.is_empty() {
            nodes.push(PipewireNode {
                id: id.to_string(),
                name,
                active,
            });
        }
    }
    nodes
}

fn refresh_companion_ai_activity(status: &gtk::Label, wave: &gtk::Label) {
    let speaking = send_speech_request(&SpeechIpcRequest::status())
        .map(|state| state.status == "speaking")
        .unwrap_or(false);
    let listening = send_microphone_request(&MicrophoneIpcRequest::Status)
        .map(|state| state.status == "listening")
        .unwrap_or(false);
    let active_requests = match send_ai_request(&AiIpcRequest::Status) {
        Ok(AiIpcResponse::Status { status }) => status.active_requests,
        _ => 0,
    };
    match (speaking, listening, active_requests) {
        (true, _, _) => {
            status.set_text("◉  Speaking");
            wave.set_text("▁▃▆█▆▃▁  ▂▅█▅▂  ▁▃▆█▆▃▁");
        }
        (_, true, _) => {
            status.set_text("●  Listening");
            wave.set_text("▁▂▃▄▅▆▇█▇▆▅▄▃▂▁  ▂▄▆█▆▄▂");
        }
        (_, _, requests) if requests > 0 => {
            status.set_text("◌  Thinking");
            wave.set_text("·  ▁▂▃▄▅▆▅▄▃▂▁  ·");
        }
        _ => {
            status.set_text("○  AI ready");
            wave.set_text("·  ▁▁▂▁▁  ·");
        }
    }
}

fn companion_wake_word_label() -> String {
    let voice = load_settings().voice;
    if voice.wake_word_enabled {
        format!("🎙  Wake word: {}", voice.wake_word)
    } else {
        "🎙  Wake word off".into()
    }
}

fn open_companion_voice_preferences(parent: &gtk::ApplicationWindow, wake_word_label: &gtk::Label) {
    let voice = load_settings().voice;
    let dialog = gtk::Dialog::builder()
        .modal(true)
        .transient_for(parent)
        .title("Voice preferences")
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Save", gtk::ResponseType::Accept);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let description = gtk::Label::new(Some(
        "Choose the phrase used by ambient voice sessions. Enabling it here does not start microphone capture.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    content.append(&description);

    let enabled = gtk::CheckButton::with_label("Enable wake word for ambient voice");
    enabled.set_active(voice.wake_word_enabled);
    content.append(&enabled);

    let wake_word = gtk::Entry::new();
    wake_word.set_text(&voice.wake_word);
    wake_word.set_placeholder_text(Some("For example: hello focaldesk"));
    wake_word.set_max_length(80);
    wake_word.set_hexpand(true);
    content.append(&companion_label("Wake word", "companion-control-label"));
    content.append(&wake_word);

    let wake_word_for_response = wake_word.clone();
    let enabled_for_response = enabled.clone();
    let label_for_response = wake_word_label.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let phrase = wake_word_for_response.text().trim().to_string();
            if phrase.is_empty() {
                wake_word_for_response.set_text("hello focaldesk");
                return;
            }
            let mut settings = load_settings();
            settings.voice.wake_word_enabled = enabled_for_response.is_active();
            settings.voice.wake_word = phrase;
            if let Err(error) = save_settings(&settings) {
                flog(format!("save Companion voice preferences failed: {error}"));
            } else {
                label_for_response.set_text(&companion_wake_word_label());
            }
        }
        dialog.close();
    });
    dialog.present();
}

#[derive(Default)]
struct CompanionDictation {
    lease_id: Option<String>,
    after_sequence: u64,
    final_text: Option<String>,
}

fn refresh_companion_conversation(
    transcript: &gtk::Label,
    response: &gtk::Label,
    dictation: &Rc<RefCell<CompanionDictation>>,
    message: &gtk::Entry,
    use_transcription: &gtk::Button,
    read_replies: &gtk::CheckButton,
    spoken_response: &Rc<RefCell<Option<String>>>,
) {
    let mut dictation = dictation.borrow_mut();
    if let Some(lease_id) = dictation.lease_id.clone() {
        match send_microphone_request(&MicrophoneIpcRequest::Poll {
            lease_id,
            after_sequence: dictation.after_sequence,
        }) {
            Ok(state) => {
                dictation.after_sequence = state.latest_sequence.max(dictation.after_sequence);
                for record in state.events {
                    match record.event {
                        MicrophoneEvent::Partial(text) => transcript.set_text(&text),
                        MicrophoneEvent::Final(text) => {
                            transcript.set_text(&text);
                            message.set_text(&text);
                            dictation.final_text = Some(text);
                            use_transcription.set_sensitive(true);
                        }
                        MicrophoneEvent::Ready => {
                            dictation.final_text = None;
                            use_transcription.set_sensitive(false);
                            transcript.set_text("Listening…");
                        }
                        MicrophoneEvent::Stopped => {
                            dictation.lease_id = None;
                            transcript.set_text("Dictation stopped.");
                        }
                        MicrophoneEvent::Error(error) => {
                            transcript.set_text(&format!("Mic error: {error}"))
                        }
                        MicrophoneEvent::VoiceActivity(_)
                        | MicrophoneEvent::WakeDetected
                        | MicrophoneEvent::Command(_) => {}
                    }
                }
            }
            Err(error) => {
                dictation.lease_id = None;
                transcript.set_text(&format!("Dictation disconnected: {error}"));
            }
        }
    } else {
        drop(dictation);
        let mic = send_microphone_request(&MicrophoneIpcRequest::Status).ok();
        if let Some(mic) = mic {
            if mic.status == "listening" {
                transcript.set_text("Microphone is in use by another FocalDesk client.");
            } else {
                transcript.set_text("Tap Talk to start live transcription.");
            }
        } else {
            transcript.set_text("Microphone service is unavailable.");
        }
        return refresh_companion_response(response, read_replies, spoken_response);
    }

    drop(dictation);
    refresh_companion_response(response, read_replies, spoken_response);
}

fn refresh_companion_response(
    response: &gtk::Label,
    read_replies: &gtk::CheckButton,
    spoken_response: &Rc<RefCell<Option<String>>>,
) {
    match send_ai_request(&AiIpcRequest::Status) {
        Ok(AiIpcResponse::Status { status }) => {
            if let Some(chat) = status.latest_chat {
                response.set_text(&chat.response);
                let changed = spoken_response.borrow().as_deref() != Some(chat.response.as_str());
                if changed {
                    *spoken_response.borrow_mut() = Some(chat.response.clone());
                    if read_replies.is_active() {
                        let text = chat.response;
                        thread::spawn(move || {
                            let _ = send_speech_request(&SpeechIpcRequest {
                                command: Some(SpeechCommand::Speak),
                                text: Some(text),
                                priority: Default::default(),
                                replace: false,
                            });
                        });
                    }
                }
            } else if status.active_requests > 0 {
                response.set_text("Generating response…");
            } else {
                response.set_text("No AI response received in this session.");
            }
        }
        _ => response.set_text("AI service is unavailable."),
    }
}

#[derive(Default)]
struct CompanionMedia {
    service: String,
    title: String,
    artist: String,
    album: String,
    art_url: Option<String>,
    position_micros: i64,
    length_micros: i64,
    playing: bool,
}

fn refresh_companion_media(
    title: &gtk::Label,
    artist: &gtk::Label,
    position: &gtk::Label,
    art: &gtk::Picture,
    selected_service: &Rc<RefCell<Option<String>>>,
) {
    let Some(media) = companion_media() else {
        title.set_text("Nothing playing");
        artist.set_text("Open a compatible media player to begin.");
        position.set_text("");
        art.set_paintable(Option::<&gdk::Paintable>::None);
        *selected_service.borrow_mut() = None;
        return;
    };
    title.set_text(&media.title);
    let detail = [media.artist.as_str(), media.album.as_str()]
        .iter()
        .filter(|value| !value.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" — ");
    artist.set_text(&detail);
    position.set_text(&format!(
        "{}  {} / {}",
        if media.playing { "●" } else { "Ⅱ" },
        format_media_duration(media.position_micros),
        format_media_duration(media.length_micros),
    ));
    if let Some(url) = media.art_url {
        let file = gio::File::for_uri(&url);
        if let Ok(texture) = gdk::Texture::from_file(&file) {
            art.set_paintable(Some(&texture));
        }
    } else {
        art.set_paintable(Option::<&gdk::Paintable>::None);
    }
    *selected_service.borrow_mut() = Some(media.service);
}

fn companion_media() -> Option<CompanionMedia> {
    let connection = DbusConnection::session().ok()?;
    let bus = DbusProxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .ok()?;
    let names: Vec<String> = bus.call("ListNames", &()).ok()?;
    let mut fallback = None;
    for service in names
        .into_iter()
        .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
    {
        let Some(media) = companion_media_for_service(&connection, service) else {
            continue;
        };
        if media.playing {
            return Some(media);
        }
        fallback.get_or_insert(media);
    }
    fallback
}

fn companion_media_for_service(
    connection: &DbusConnection,
    service: String,
) -> Option<CompanionMedia> {
    let proxy = DbusProxy::new(
        connection,
        service.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )
    .ok()?;
    let metadata: HashMap<String, OwnedValue> = proxy.get_property("Metadata").ok()?;
    let status: String = proxy.get_property("PlaybackStatus").unwrap_or_default();
    let position_micros: i64 = proxy.get_property("Position").unwrap_or(0);
    let title = metadata_string(&metadata, "xesam:title").unwrap_or_else(|| "Unknown title".into());
    let artist = metadata_strings(&metadata, "xesam:artist").join(", ");
    drop(proxy);
    Some(CompanionMedia {
        service,
        title,
        artist,
        album: metadata_string(&metadata, "xesam:album").unwrap_or_default(),
        art_url: metadata_string(&metadata, "mpris:artUrl"),
        position_micros,
        length_micros: metadata_i64(&metadata, "mpris:length").unwrap_or(0),
        playing: status == "Playing",
    })
}

fn metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(str::to_owned)
}

fn metadata_strings(metadata: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .unwrap_or_default()
}

fn metadata_i64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    metadata
        .get(key)
        .and_then(|value| i64::try_from(value).ok())
}

fn media_player_command(service: &str, method: &str) -> Result<()> {
    let connection = DbusConnection::session()?;
    let proxy = DbusProxy::new(
        &connection,
        service,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )?;
    proxy.call_method(method, &())?;
    Ok(())
}

fn format_media_duration(micros: i64) -> String {
    let seconds = (micros.max(0) / 1_000_000) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn pipewire_volume_percent() -> Option<f64> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .find_map(|word| word.parse::<f64>().ok())
        .map(|volume| (volume * 100.0).clamp(0.0, 100.0))
}

fn set_pipewire_volume(percent: f64) -> Result<(), String> {
    let percent = percent.round().clamp(0.0, 100.0);
    let status = Command::new("wpctl")
        .args([
            "set-volume",
            "@DEFAULT_AUDIO_SINK@",
            &format!("{percent:.0}%"),
        ])
        .status()
        .map_err(|error| format!("run wpctl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("wpctl exited with {status}"))
    }
}

fn viewsonic_brightness_percent() -> Option<f64> {
    let connector = desktop_snapshot()?
        .outputs
        .into_iter()
        .find_map(|output| is_viewsonic_td1655(&output).then_some(output.connector))?;
    ddc_brightness_percent(&connector)
}

fn ddc_brightness_percent(connector: &str) -> Option<f64> {
    let output = Command::new("sudo")
        .args([
            "-n",
            "/usr/local/libexec/focaldesk-viewsonic-brightness",
            "get",
            connector,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let current = text
        .split("current value =")
        .nth(1)?
        .split(',')
        .next()?
        .trim();
    current.parse::<f64>().ok()
}

fn set_viewsonic_brightness(percent: f64) -> Result<(), String> {
    let connector = desktop_snapshot()
        .and_then(|snapshot| {
            snapshot
                .outputs
                .into_iter()
                .find_map(|output| is_viewsonic_td1655(&output).then_some(output.connector))
        })
        .ok_or_else(|| "ViewSonic TD1655 connector is unavailable".to_string())?;
    set_ddc_brightness(&connector, percent)
}

fn set_ddc_brightness(connector: &str, percent: f64) -> Result<(), String> {
    let value = percent.round().clamp(0.0, 100.0).to_string();
    let status = Command::new("sudo")
        .args([
            "-n",
            "/usr/local/libexec/focaldesk-viewsonic-brightness",
            "set",
            connector,
            &value,
        ])
        .status()
        .map_err(|error| format!("run DDC/CI brightness command: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("DDC/CI brightness command exited with {status}"))
    }
}

fn refresh_companion_clock(
    header_clock: &gtk::Label,
    large_clock: &gtk::Label,
    date_label: &gtk::Label,
) {
    let now = chrono::Local::now();
    // Share the panel's existing civilian/military time preference. Read it
    // at refresh time so changing the setting updates the Companion too.
    let pattern = match load_config().panel.clock_format {
        ClockFormat::TwelveHour => "%-I:%M %p",
        ClockFormat::TwentyFourHour => "%H:%M",
    };
    let time = now.format(pattern).to_string();
    header_clock.set_text(&time);
    large_clock.set_text(&time);
    date_label.set_text(&now.format("%a, %b %-d, %Y").to_string());
}

#[derive(Clone, Debug)]
struct CompanionWeather {
    icon: &'static str,
    condition: String,
    temperature: String,
    details: String,
}

impl CompanionWeather {
    fn message(
        icon: &'static str,
        condition: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            icon,
            condition: condition.into(),
            temperature: String::new(),
            details: details.into(),
        }
    }
}

/// Start a deliberately low-frequency forecast query for the Companion.
///
/// Coordinates are already an opt-in local setting used by automatic night
/// light scheduling.  We never infer a location from an IP address, and do
/// not send a request when Location Services has been disabled.
fn start_companion_weather_updates() -> Arc<Mutex<CompanionWeather>> {
    let settings = load_settings();
    let initial = if !settings.privacy.location_services {
        CompanionWeather::message(
            "⌖",
            "Location services are off",
            "Enable Location Services to show weather.",
        )
    } else if settings.accessibility.night_light.latitude.is_none()
        || settings.accessibility.night_light.longitude.is_none()
    {
        CompanionWeather::message(
            "⌖",
            "Set a local location",
            "Add coordinates in Display & Comfort to show weather.",
        )
    } else {
        CompanionWeather::message("◌", "Loading local weather…", "")
    };
    let state = Arc::new(Mutex::new(initial));

    if !settings.privacy.location_services {
        return state;
    }
    let (Some(latitude), Some(longitude)) = (
        settings.accessibility.night_light.latitude,
        settings.accessibility.night_light.longitude,
    ) else {
        return state;
    };

    let state_for_worker = state.clone();
    thread::spawn(move || loop {
        let next = fetch_companion_weather(latitude, longitude).unwrap_or_else(|error| {
            flog(format!("Companion weather refresh failed: {error}"));
            CompanionWeather::message("!", "Weather unavailable", "Will try again shortly.")
        });
        if let Ok(mut current) = state_for_worker.lock() {
            *current = next;
        }
        // Forecast values change slowly, and this avoids making the dashboard
        // a high-frequency location-sharing client.
        thread::sleep(Duration::from_secs(15 * 60));
    });
    state
}

fn fetch_companion_weather(latitude: f64, longitude: f64) -> Result<CompanionWeather, String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude:.4}&longitude={longitude:.4}&current=temperature_2m,relative_humidity_2m,wind_speed_10m,weather_code&temperature_unit=fahrenheit&wind_speed_unit=mph"
    );
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            &url,
        ])
        .output()
        .map_err(|error| format!("start forecast request: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode forecast response: {error}"))?;
    let current = response
        .get("current")
        .ok_or_else(|| "forecast response did not include current conditions".to_string())?;
    let temperature = current
        .get("temperature_2m")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "forecast response did not include temperature".to_string())?;
    let humidity = current
        .get("relative_humidity_2m")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "forecast response did not include humidity".to_string())?;
    let wind_speed = current
        .get("wind_speed_10m")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "forecast response did not include wind speed".to_string())?;
    let code = current
        .get("weather_code")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "forecast response did not include weather condition".to_string())?;
    let (icon, condition) = companion_weather_condition(code, wind_speed);
    Ok(CompanionWeather {
        icon,
        condition: condition.into(),
        temperature: format!("{temperature:.0}°F"),
        details: format!("Humidity {:.0}%   ·   Wind {:.0} mph", humidity, wind_speed),
    })
}

fn companion_weather_condition(code: i64, wind_speed_mph: f64) -> (&'static str, &'static str) {
    // WMO weather codes describe precipitation and cloud cover, not wind.
    // Give strong wind its own immediately recognizable condition when it is
    // the principal condition, while retaining rain/snow/storm warnings.
    if wind_speed_mph >= 20.0 && !matches!(code, 51..=57 | 61..=67 | 71..=77 | 80..=86 | 95..=99) {
        return ("≋", "Windy");
    }
    match code {
        0 => ("☀", "Sunny"),
        1 | 2 => ("⛅", "Partly cloudy"),
        3 => ("☁", "Cloudy"),
        45 | 48 => ("≋", "Foggy"),
        51 | 53 | 55 | 56 | 57 => ("♧", "Drizzle"),
        61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => ("☂", "Rainy"),
        71 | 73 | 75 | 77 | 85 | 86 => ("❄", "Snowy"),
        95 | 96 | 99 => ("ϟ", "Thunderstorms"),
        _ => ("◌", "Current conditions"),
    }
}

fn refresh_companion_weather(
    icon: &gtk::Label,
    condition: &gtk::Label,
    temperature: &gtk::Label,
    details: &gtk::Label,
    state: &Arc<Mutex<CompanionWeather>>,
) {
    let Ok(weather) = state.lock() else {
        return;
    };
    icon.set_text(weather.icon);
    condition.set_text(&weather.condition);
    temperature.set_text(&weather.temperature);
    details.set_text(&weather.details);
}

fn companion_card(title: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("companion-card");
    card.append(&companion_label(title, "companion-card-title"));
    card
}

fn companion_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class(class);
    label
}

fn companion_field(text: &str) -> gtk::Label {
    let field = companion_label(text, "companion-field");
    field.set_vexpand(true);
    field
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

    if vui_client::enabled(ShellRole::Dock) {
        if vui_client::build(&window, ShellRole::Dock, connector.clone()).is_some() {
            return window;
        }
        focaldesk_logging::flog_warn!(
            "VUI dock client initialization failed; using GTK dock fallback"
        );
    } else {
        focaldesk_logging::flog_info!(
            "VUI dock client disabled for {connector}; using GTK dock fallback"
        );
    }
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

    // Keep every launcher together.  Windows that appear because an app has
    // been launched are tasks, not launch controls, so they belong in their
    // own trailing group instead of being inserted between pinned controls.
    let running_separator = shelf_separator();
    running_separator.set_visible(false);
    shelf.append(&running_separator);
    let running = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    running.add_css_class("shelf-group");
    running.add_css_class("running-group");
    shelf.append(&running);

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
                    &running_separator,
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
    separator: &gtk::Separator,
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
        separator.set_visible(false);
        return;
    }
    container.set_visible(true);
    separator.set_visible(true);
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
        let icon = app_icon_name(Some(identity));
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
                    x11: window.x11,
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
        if let Some(window) = windows.iter().find(|window| window.x11) {
            attach_x11_terminate_context_menu(&button, window.id);
        }
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
                    x11: window.x11,
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
    x11: bool,
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
        if entry.x11 {
            attach_x11_terminate_context_menu(&row, entry.id);
        }
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

fn attach_x11_terminate_context_menu(widget: &impl IsA<gtk::Widget>, window_id: u32) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(parent) = gesture.widget() else {
            return;
        };
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        popover.set_parent(&parent);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| popover.unparent());

        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        menu.set_margin_start(6);
        menu.set_margin_end(6);
        let terminate = gtk::Button::with_label("Terminate application");
        terminate.add_css_class("flat");
        terminate.add_css_class("destructive-action");
        terminate.set_halign(gtk::Align::Fill);
        let popover_for_action = popover.clone();
        terminate.connect_clicked(move |_| {
            popover_for_action.popdown();
            send_action(DesktopAction::TerminateX11Application { window_id });
        });
        menu.append(&terminate);
        popover.set_child(Some(&menu));
        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    widget.add_controller(click);
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
    if identity.contains("focaldesk-ai-console") || identity.contains("focaldesk.aiconsole") {
        "focaldesk-ai-console"
    } else if identity.contains("terminal") || identity.contains("foot") {
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

fn ai_console_identity_matches(app_id: Option<&str>, class: Option<&str>, title: &str) -> bool {
    app_id.into_iter().chain(class).any(|identity| {
        let identity = identity.to_ascii_lowercase();
        identity.contains("focaldesk-ai-console") || identity.contains("focaldesk.aiconsole")
    }) || title.eq_ignore_ascii_case("FocalDesk AI Console")
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
    ai_console_button: gtk::Button,
    ai_console_target: Rc<Cell<Option<u32>>>,
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
    updates_button: gtk::Button,
    updates_image: gtk::Image,
    clock: gtk::Label,
    workspaces: Rc<RefCell<RailWorkspaceWidgets>>,
    workspace_box: gtk::Box,
    add_workspace_button: gtk::Button,
    remove_workspace_button: gtk::Button,
    last_state: RefCell<Option<SystemRailState>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SystemRailOutputState {
    connector: String,
    focused: bool,
    hdr_supported: bool,
    hdr_requested: bool,
    hdr_active: bool,
    wide_gamut_active: bool,
    icc_lut_fallback_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SystemRailAiState {
    id: u32,
    minimized: bool,
    focused: bool,
}

/// The subset of a desktop snapshot that can affect the system rail.
///
/// Desktop snapshots currently arrive through request/response IPC. Keeping a
/// compact signature here makes that fallback polling edge-triggered from the
/// GTK UI's perspective: unchanged snapshots perform no widget or paint work.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SystemRailState {
    output: Option<SystemRailOutputState>,
    ai: Option<SystemRailAiState>,
    active_workspace: u32,
    workspace_count: usize,
    max_workspace_slots: usize,
    network_carrier: bool,
    microphone_active: bool,
    camera_active: bool,
    battery_percent: Option<u8>,
    externally_powered: bool,
    line_power_online: Option<bool>,
    battery_charging: bool,
    notification_unread_count: usize,
    update_available_count: usize,
    update_busy: bool,
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

    if vui_client::enabled(ShellRole::Panel) {
        if vui_client::build(&window, ShellRole::Panel, connector.clone()).is_some() {
            return window;
        }
        focaldesk_logging::flog_warn!(
            "VUI rail client initialization failed; using GTK rail fallback"
        );
    } else {
        focaldesk_logging::flog_info!(
            "VUI rail client disabled for {connector}; using GTK rail fallback"
        );
    }

    // Legacy GTK rail implementation used when VUI is disabled or unavailable.
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

    let primary = gtk::Box::new(gtk::Orientation::Vertical, 4);
    primary.add_css_class("rail-group");
    let ai_console_target = Rc::new(Cell::new(None::<u32>));
    let click_ai_console_target = ai_console_target.clone();
    let (ai_console_button, _) = glass_icon_button(
        "focaldesk-ai-console",
        "Open FocalDesk AI Console",
        "rail-button",
        move || {
            if let Some(window_id) = click_ai_console_target.get() {
                send_action(DesktopAction::FocusWindow { window_id });
            } else {
                send_action(DesktopAction::LaunchApp {
                    app: "@ai-console".into(),
                });
            }
        },
    );
    ai_console_button.add_css_class("rail-primary-action");
    primary.append(&ai_console_button);
    let quick_settings_connector = connector.clone();
    primary.append(
        &glass_icon_button(
            "preferences-system-symbolic",
            "Quick Settings",
            "rail-button",
            move || open_shell_panel(&quick_settings_connector, ShellPanel::QuickSettings),
        )
        .0,
    );
    rail.append(&primary);

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
    battery_button.connect_clicked(|_| {
        send_action(DesktopAction::OpenSettingsPanel {
            panel: "power".into(),
        });
    });
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
    let updates_connector = connector.clone();
    let (updates_button, updates_image) = glass_icon_button(
        "software-update-available-symbolic",
        "System updates",
        "rail-button",
        move || open_shell_panel(&updates_connector, ShellPanel::Updates),
    );
    bottom.append(&updates_button);
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
        ai_console_button,
        ai_console_target,
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
        updates_button,
        updates_image,
        clock,
        workspaces,
        workspace_box,
        add_workspace_button,
        remove_workspace_button,
        last_state: RefCell::new(None),
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
    let state = system_rail_state(snapshot, connector);
    {
        let mut last_state = widgets.last_state.borrow_mut();
        if last_state.as_ref() == Some(&state) {
            return;
        }
        *last_state = Some(state);
    }

    let output = output_for_connector(snapshot, connector);
    widgets
        .focus_notch
        .set_visible(output.is_some_and(|output| output.focused));
    let ai_window = snapshot.windows.iter().find(|window| {
        window.mapped
            && ai_console_identity_matches(
                window.app_id.as_deref(),
                window.class.as_deref(),
                &window.title,
            )
    });
    widgets.ai_console_target.set(
        ai_window
            .filter(|window| !window.minimized)
            .map(|window| window.id),
    );
    set_button_active(&widgets.ai_console_button, ai_window.is_some());
    widgets.ai_console_button.set_tooltip_text(Some(
        if ai_window.is_some_and(|window| window.focused) {
            "FocalDesk AI Console is active"
        } else if ai_window.is_some() {
            "Focus FocalDesk AI Console"
        } else {
            "Open FocalDesk AI Console"
        },
    ));
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
    let externally_powered = snapshot.shell.line_power_online == Some(true)
        || (snapshot.shell.line_power_online.is_none() && snapshot.shell.battery_percent.is_none());
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
        snapshot.shell.line_power_online,
        snapshot.shell.battery_charging,
    ) {
        (Some(percent), Some(true), true) => format!("Battery {percent}% · charging"),
        (Some(percent), Some(true), false) => format!("Battery {percent}% · plugged in"),
        (Some(percent), Some(false), _) => format!("Battery {percent}% · on battery"),
        (Some(percent), None, true) => format!("Battery {percent}% · charging · source unknown"),
        (Some(percent), None, false) => format!("Battery {percent}% · source unknown"),
        (None, Some(true), _) => "External power".into(),
        (None, Some(false), _) => "Battery unavailable · on battery".into(),
        (None, None, _) => "External power (assumed; no battery reported)".into(),
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
    set_shell_icon(
        &widgets.updates_image,
        if snapshot.shell.update_busy {
            "emblem-synchronizing-symbolic"
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
}

fn system_rail_state(snapshot: &DesktopSnapshot, connector: &str) -> SystemRailState {
    let output = output_for_connector(snapshot, connector);
    let ai = snapshot
        .windows
        .iter()
        .find(|window| {
            window.mapped
                && ai_console_identity_matches(
                    window.app_id.as_deref(),
                    window.class.as_deref(),
                    &window.title,
                )
        })
        .map(|window| SystemRailAiState {
            id: window.id,
            minimized: window.minimized,
            focused: window.focused,
        });
    let externally_powered = snapshot.shell.line_power_online == Some(true)
        || (snapshot.shell.line_power_online.is_none() && snapshot.shell.battery_percent.is_none());

    SystemRailState {
        output: output.map(|output| SystemRailOutputState {
            connector: output.connector.clone(),
            focused: output.focused,
            hdr_supported: output.hdr_supported,
            hdr_requested: output.hdr_requested,
            hdr_active: output.hdr_active,
            wide_gamut_active: output.wide_gamut_active,
            icc_lut_fallback_active: output.icc_lut_fallback_active,
        }),
        ai,
        active_workspace: output
            .map(|output| output.active_workspace_id)
            .unwrap_or(snapshot.session.active_workspace_id)
            .max(1),
        workspace_count: snapshot.shell.workspace_count.max(1),
        max_workspace_slots: snapshot.shell.max_workspace_slots.max(1),
        network_carrier: snapshot.shell.network_carrier,
        microphone_active: snapshot.shell.microphone_active,
        camera_active: snapshot.shell.camera_active,
        battery_percent: snapshot.shell.battery_percent,
        externally_powered,
        line_power_online: snapshot.shell.line_power_online,
        battery_charging: snapshot.shell.battery_charging,
        notification_unread_count: snapshot.shell.notification_unread_count,
        update_available_count: snapshot.shell.update_available_count,
        update_busy: snapshot.shell.update_busy,
    }
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
        let size = if icon_name == "focaldesk-ai-console" {
            30
        } else {
            26
        };
        image.set_pixel_size(size as i32);
        set_shell_icon_at_size(&image, icon_name, size);
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
    let cache_key = (icon_name.to_string(), icon_size);
    if let Some(texture) = SHELL_ICON_CACHE.with(|cache| cache.borrow().get(&cache_key).cloned()) {
        image.set_paintable(Some(&texture));
        return;
    }
    let Ok(svg) = std::str::from_utf8(svg) else {
        return;
    };
    let tint = if icon_name == "focaldesk-ai-console" {
        "#D6E9FF"
    } else {
        "#8CA4C4"
    };
    let styled = svg.replace("currentColor", tint);
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
    let texture: gdk::Texture = gdk::MemoryTexture::new(
        icon_size as i32,
        icon_size as i32,
        gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        (icon_size * 4) as usize,
    )
    .upcast();
    image.set_paintable(Some(&texture));
    SHELL_ICON_CACHE.with(|cache| {
        cache.borrow_mut().insert(cache_key, texture);
    });
}

fn focaldesk_icon_svg(icon_name: &str) -> Option<&'static [u8]> {
    Some(match icon_name {
        "focaldesk-ai-console" => include_bytes!("../../../assets/icons/bot.svg"),
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
        match send_desktop_request(&IpcRequest::ExecuteDesktopAction {
            action: action.clone(),
        }) {
            Ok(IpcResponse::Ok) => {}
            Ok(IpcResponse::Error { message }) => {
                flog(format!("shell action {action:?} was rejected: {message}"));
            }
            Ok(response) => {
                flog(format!(
                    "shell action {action:?} returned unexpected response: {response:?}"
                ));
            }
            Err(error) => flog(format!("shell action {action:?} failed: {error}")),
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
    panel_darkness: Option<f64>,
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
        panel_darkness: config.appearance.panel_darkness,
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
            panel_darkness: defaults.appearance.panel_darkness,
        },
    )
}

fn shell_css_configured(theme: &FlowTheme, snapshot: &ThemeSnapshot) -> String {
    let background = theme.chrome.bg_color;
    let surface = theme.chrome.panel_color;
    let accent = theme.chrome.accent_color;
    let trim = theme.chrome.trim_color;
    let dock_backing_opacity = snapshot
        .panel_darkness
        .map(|value| value.clamp(0.0, 1.0) as f32)
        .unwrap_or_else(|| theme.backing.dock_opacity.clamp(0.0, 1.0));
    let definitions = [
        ("fd_shell_bg", background),
        ("fd_shell_backing", [0.025, 0.035, 0.055, 1.0]),
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
    let dock_backing_opacity = format!("{dock_backing_opacity:.3}");

    format!(
        "{definitions}\n{SHELL_CSS_BASE}\n\
         window.focal-shell-window {{ font-size: {font_scale:.3}em; }}\n\
         window.focal-panel-window > .focal-panel {{ border-radius: {panel_corners}; {panel_edge} }}\n\
         .focal-dock {{ border-radius: {dock_corners}; border-width: {border_width:.1}px; }}\n\
         .dock-module {{ min-height: {dock_control_size}px; border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         .panel-launcher, .panel-well, .panel-status-button {{ border-radius: {radius:.1}px; border-width: {border_width:.1}px; }}\n\
         .system-rail {{ background: transparent; }}\n\
         .task-shelf {{ background-color: alpha(@fd_shell_backing, {dock_backing_opacity}); background-image: linear-gradient(155deg, alpha(@fd_shell_highlight, 0.18), alpha(@fd_shell_backing, {dock_backing_opacity}) 22%, alpha(@fd_shell_backing, {dock_backing_opacity}) 62%, alpha(@fd_shell_backing, {dock_backing_opacity})); }}\n\
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
        ai_console_identity_matches, app_icon_name, companion_weather_condition,
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
    fn weather_conditions_use_clear_icons_and_preserve_precipitation_warnings() {
        assert_eq!(companion_weather_condition(0, 5.0), ("☀", "Sunny"));
        assert_eq!(companion_weather_condition(3, 5.0), ("☁", "Cloudy"));
        assert_eq!(companion_weather_condition(63, 5.0), ("☂", "Rainy"));
        assert_eq!(companion_weather_condition(73, 5.0), ("❄", "Snowy"));
        assert_eq!(companion_weather_condition(0, 25.0), ("≋", "Windy"));
        assert_eq!(companion_weather_condition(63, 25.0), ("☂", "Rainy"));
    }

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
                panel_darkness: None,
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
                panel_darkness: None,
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
    fn ai_console_window_uses_the_dedicated_icon_and_identity() {
        assert!(ai_console_identity_matches(
            Some("dev.focaldesk.AiConsole"),
            None,
            "Untitled"
        ));
        assert!(ai_console_identity_matches(
            None,
            Some("focaldesk-ai-console"),
            "Untitled"
        ));
        assert!(!ai_console_identity_matches(
            Some("org.gnome.Console"),
            None,
            "Console"
        ));
        assert_eq!(
            app_icon_name(Some("dev.focaldesk.AiConsole")),
            "focaldesk-ai-console"
        );
    }

    #[test]
    fn chooser_never_repeats_an_unhelpful_untitled_label() {
        let entry = ShelfOverflowEntry {
            id: 7,
            identity: "untitled".into(),
            title: "Untitled".into(),
            focused: false,
            x11: false,
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
            refresh_mhz: 60_000,
            x,
            y: 0,
            scale: 1.0,
            transform: focaldesk_settings_core::DisplayTransform::Normal,
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
        }
    }
}
