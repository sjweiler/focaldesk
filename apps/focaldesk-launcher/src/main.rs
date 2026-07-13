use adw::prelude::*;
use focaldesk_logging::init_default_logging;
use focaldesk_settings_core::load_settings;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use tracing::warn;

#[derive(Clone)]
struct LauncherApp {
    info: gio::AppInfo,
    name: String,
    category: &'static str,
}

const CATEGORIES: [&str; 8] = [
    "All",
    "Development",
    "Internet",
    "Office",
    "Media",
    "Games",
    "System",
    "Utilities",
];

fn sibling_binary(name: &str) -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(name)))
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

fn spawn(command: &str) {
    if let Err(err) = Command::new(command).spawn() {
        warn!(command, %err, "focaldesk-launcher: failed to launch app");
    }
}

fn app_category(categories: Option<glib::GString>) -> &'static str {
    let categories = categories.as_deref().unwrap_or_default();
    let has = |names: &[&str]| {
        categories
            .split(';')
            .any(|category| names.contains(&category))
    };

    if has(&["Development", "IDE", "GUIDesigner", "WebDevelopment"]) {
        "Development"
    } else if has(&["Network", "WebBrowser", "Email", "Chat", "InstantMessaging"]) {
        "Internet"
    } else if has(&["Office", "WordProcessor", "Spreadsheet", "Presentation"]) {
        "Office"
    } else if has(&["AudioVideo", "Audio", "Video", "Graphics", "Photography"]) {
        "Media"
    } else if has(&["Game"]) {
        "Games"
    } else if has(&["System", "Settings", "Security"]) {
        "System"
    } else {
        "Utilities"
    }
}

fn installed_apps() -> Vec<LauncherApp> {
    let mut apps: Vec<_> = gio::AppInfo::all()
        .into_iter()
        .filter(|info| info.should_show())
        .filter(|info| info.id().as_deref() != Some("com.focaldesk.Launcher.desktop"))
        .map(|info| {
            let category = info
                .clone()
                .downcast::<gio::DesktopAppInfo>()
                .ok()
                .and_then(|desktop| desktop.categories());
            LauncherApp {
                name: info.display_name().to_string(),
                info,
                category: app_category(category),
            }
        })
        .collect();

    apps.sort_by_key(|app| app.name.to_lowercase());
    apps.dedup_by(|a, b| a.info.id() == b.info.id() && a.name == b.name);
    apps
}

const LAUNCH_ENV_KEYS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// Wine and Proton applications need to use XWayland rather than inheriting
/// the native Wayland display from the launcher.
fn apply_launch_environment(context: &impl IsA<gio::AppLaunchContext>, force_x11: bool) {
    for key in LAUNCH_ENV_KEYS {
        if force_x11 && *key == "WAYLAND_DISPLAY" {
            context.unsetenv(key);
        } else if let Some(value) = std::env::var_os(key) {
            context.setenv(key, value);
        } else {
            context.unsetenv(key);
        }
    }
}

fn app_targets_exe(info: &gio::AppInfo) -> bool {
    let desktop = info.clone().downcast::<gio::DesktopAppInfo>().ok();
    desktop
        .as_ref()
        .and_then(|desktop| desktop.string("Exec"))
        .is_some_and(|exec| exec.to_ascii_lowercase().contains(".exe"))
        || info
            .commandline()
            .and_then(|command| command.to_str().map(str::to_ascii_lowercase))
            .is_some_and(|command| command.contains(".exe"))
}

fn app_button(app: &LauncherApp, window: &adw::ApplicationWindow) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_size_request(132, 92);
    button.set_tooltip_text(Some(&app.name));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_valign(gtk::Align::Center);
    let image = app
        .info
        .icon()
        .map(|icon| gtk::Image::from_gicon(&icon))
        .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-executable-symbolic"));
    image.set_pixel_size(40);
    let label = gtk::Label::new(Some(&app.name));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(16);
    content.append(&image);
    content.append(&label);
    button.set_child(Some(&content));

    let info = app.info.clone();
    let window = window.clone();
    button.connect_clicked(move |_| {
        let context = gtk::prelude::WidgetExt::display(&window).app_launch_context();
        apply_launch_environment(&context, app_targets_exe(&info));
        if let Err(err) = info.launch(&[], Some(&context)) {
            warn!(app = %info.display_name(), %err, "focaldesk-launcher: failed to launch desktop app");
        } else {
            window.close();
        }
    });
    button
}

fn render_apps(
    flow: &gtk::FlowBox,
    apps: &[LauncherApp],
    category: &str,
    query: &str,
    window: &adw::ApplicationWindow,
) {
    while let Some(child) = flow.first_child() {
        flow.remove(&child);
    }

    let query = query.trim().to_lowercase();
    for app in apps.iter().filter(|app| {
        (category == "All" || app.category == category)
            && (query.is_empty() || app.name.to_lowercase().contains(&query))
    }) {
        flow.insert(&app_button(app, window), -1);
    }
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Launcher"));
    window.set_default_size(720, 620);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&root));
    window.set_content(Some(&toolbar_view));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search applications"));
    root.append(&search);

    let quick_label = gtk::Label::new(Some("Quick launch"));
    quick_label.set_halign(gtk::Align::Start);
    quick_label.add_css_class("heading");
    root.append(&quick_label);

    let quick = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entries: [(&str, &str, Box<dyn Fn() -> String>); 3] = [
        (
            "Terminal",
            "utilities-terminal-symbolic",
            Box::new(|| load_settings().apps.terminal),
        ),
        (
            "Browser",
            "web-browser-symbolic",
            Box::new(|| load_settings().apps.browser),
        ),
        (
            "Files",
            "system-file-manager-symbolic",
            Box::new(|| sibling_binary("focaldesk-files")),
        ),
    ];
    for (label, icon, resolve_command) in entries {
        let button = gtk::Button::with_label(label);
        button.set_icon_name(icon);
        button.set_height_request(42);
        button.set_hexpand(true);
        let window = window.clone();
        button.connect_clicked(move |_| {
            spawn(&resolve_command());
            window.close();
        });
        quick.append(&button);
    }
    root.append(&quick);

    let categories = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let selected_category = Rc::new(RefCell::new("All"));
    root.append(&categories);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(5);
    flow.set_row_spacing(8);
    flow.set_column_spacing(8);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&flow));
    root.append(&scroll);

    let apps = Rc::new(installed_apps());
    for category in CATEGORIES {
        let button = gtk::ToggleButton::with_label(category);
        button.set_active(category == "All");
        if let Some(previous) = categories.last_child().and_downcast::<gtk::ToggleButton>() {
            button.set_group(Some(&previous));
        }
        let apps = apps.clone();
        let flow = flow.clone();
        let search = search.clone();
        let window = window.clone();
        let selected_category = selected_category.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                *selected_category.borrow_mut() = category;
                render_apps(&flow, &apps, category, search.text().as_str(), &window);
            }
        });
        categories.append(&button);
    }

    let apps_for_search = apps.clone();
    let flow_for_search = flow.clone();
    let window_for_search = window.clone();
    search.connect_search_changed(move |entry| {
        render_apps(
            &flow_for_search,
            &apps_for_search,
            *selected_category.borrow(),
            entry.text().as_str(),
            &window_for_search,
        );
    });
    render_apps(&flow, &apps, "All", "", &window);

    let key_controller = gtk::EventControllerKey::new();
    let window_for_key = window.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            window_for_key.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(key_controller);
    window.present();
}

fn main() {
    init_default_logging();
    let app = adw::Application::new(Some("com.focaldesk.Launcher"), Default::default());
    app.connect_activate(build_ui);
    app.run();
}
