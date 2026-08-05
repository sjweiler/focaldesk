use adw::prelude::*;
use focaldesk_config::load_config;
use focaldesk_gtk::{StateKind, StateView};
use focaldesk_logging::init_default_logging;
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{gtk_app_css, gtk_app_prefers_dark, theme_by_name, GtkAppThemeOptions};
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;
use tracing::warn;

#[derive(Clone)]
struct LauncherApp {
    info: gio::AppInfo,
    id: String,
    name: String,
    category: &'static str,
}

#[derive(Debug, Default)]
struct LauncherState {
    favorites: Vec<String>,
    recents: Vec<String>,
}

const MAX_RECENTS: usize = 12;

const CATEGORIES: [&str; 10] = [
    "All",
    "Favorites",
    "Recent",
    "Development",
    "Internet",
    "Office",
    "Media",
    "Games",
    "System",
    "Utilities",
];

fn state_path() -> PathBuf {
    glib::user_config_dir()
        .join("focaldesk")
        .join("launcher-state")
}

fn load_launcher_state() -> LauncherState {
    let Ok(contents) = fs::read_to_string(state_path()) else {
        return LauncherState::default();
    };

    let mut state = LauncherState::default();
    for line in contents.lines() {
        if let Some(id) = line.strip_prefix("favorite\t") {
            if !id.is_empty() && !state.favorites.iter().any(|entry| entry == id) {
                state.favorites.push(id.to_string());
            }
        } else if let Some(id) = line.strip_prefix("recent\t") {
            if !id.is_empty() && !state.recents.iter().any(|entry| entry == id) {
                state.recents.push(id.to_string());
            }
        }
    }
    state.recents.truncate(MAX_RECENTS);
    state
}

fn save_launcher_state(state: &LauncherState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            warn!(%err, "focaldesk-launcher: failed to create state directory");
            return;
        }
    }

    let mut contents = String::new();
    for id in &state.favorites {
        contents.push_str("favorite\t");
        contents.push_str(id);
        contents.push('\n');
    }
    for id in &state.recents {
        contents.push_str("recent\t");
        contents.push_str(id);
        contents.push('\n');
    }
    if let Err(err) = fs::write(path, contents) {
        warn!(%err, "focaldesk-launcher: failed to save launcher state");
    }
}

fn remember_recent(state: &mut LauncherState, id: &str) {
    state.recents.retain(|entry| entry != id);
    state.recents.insert(0, id.to_string());
    state.recents.truncate(MAX_RECENTS);
}

fn toggle_favorite(state: &mut LauncherState, id: &str) -> bool {
    if let Some(index) = state.favorites.iter().position(|entry| entry == id) {
        state.favorites.remove(index);
        false
    } else {
        state.favorites.push(id.to_string());
        true
    }
}

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
            let name = info.display_name().to_string();
            LauncherApp {
                id: info
                    .id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| name.clone()),
                name,
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

fn app_button(
    app: &LauncherApp,
    window: &adw::ApplicationWindow,
    state: &Rc<RefCell<LauncherState>>,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("launcher-app-tile");
    button.set_size_request(132, 92);
    button.set_hexpand(true);
    button.set_tooltip_text(Some(&format!("{} · Right-click for favorite", app.name)));
    if state
        .borrow()
        .favorites
        .iter()
        .any(|favorite| favorite == &app.id)
    {
        button.add_css_class("launcher-favorite");
    }

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
    let app_id = app.id.clone();
    let state_for_launch = state.clone();
    let window_for_launch = window.clone();
    button.connect_clicked(move |_| {
        let context =
            gtk::prelude::WidgetExt::display(&window_for_launch).app_launch_context();
        apply_launch_environment(&context, app_targets_exe(&info));
        if let Err(err) = info.launch(&[], Some(&context)) {
            warn!(app = %info.display_name(), %err, "focaldesk-launcher: failed to launch desktop app");
        } else {
            let mut state = state_for_launch.borrow_mut();
            remember_recent(&mut state, &app_id);
            save_launcher_state(&state);
            window_for_launch.close();
        }
    });

    let popover = gtk::Popover::new();
    popover.set_has_arrow(true);
    popover.set_parent(&button);
    let favorite_action = gtk::Button::with_label(if button.has_css_class("launcher-favorite") {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    });
    favorite_action.add_css_class("flat");
    popover.set_child(Some(&favorite_action));

    let app_id = app.id.clone();
    let state_for_favorite = state.clone();
    let button_for_favorite = button.clone();
    let popover_for_action = popover.clone();
    favorite_action.connect_clicked(move |action| {
        let mut state = state_for_favorite.borrow_mut();
        let favorite = toggle_favorite(&mut state, &app_id);
        save_launcher_state(&state);
        if favorite {
            button_for_favorite.add_css_class("launcher-favorite");
            action.set_label("Remove from Favorites");
        } else {
            button_for_favorite.remove_css_class("launcher-favorite");
            action.set_label("Add to Favorites");
        }
        popover_for_action.popdown();
    });

    let secondary_click = gtk::GestureClick::new();
    secondary_click.set_button(gdk::BUTTON_SECONDARY);
    secondary_click.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        popover.popup();
    });
    button.add_controller(secondary_click);

    let window_for_keys = window.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        let direction = match key {
            gdk::Key::Left => Some(gtk::DirectionType::Left),
            gdk::Key::Right => Some(gtk::DirectionType::Right),
            gdk::Key::Up => Some(gtk::DirectionType::Up),
            gdk::Key::Down => Some(gtk::DirectionType::Down),
            _ => None,
        };
        if let Some(direction) = direction {
            window_for_keys.child_focus(direction);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    button.add_controller(key_controller);
    button
}

fn app_search_score(name: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name == query {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if name
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.starts_with(query))
    {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else {
        None
    }
}

fn render_apps(
    flow: &gtk::FlowBox,
    results: &gtk::Stack,
    empty_state: &StateView,
    apps: &[LauncherApp],
    category: &str,
    query: &str,
    window: &adw::ApplicationWindow,
    state: &Rc<RefCell<LauncherState>>,
) {
    while let Some(child) = flow.first_child() {
        flow.remove(&child);
    }

    let query = query.trim().to_lowercase();
    let state_snapshot = state.borrow();
    let mut matches: Vec<_> = apps
        .iter()
        .filter(|app| match category {
            "All" => true,
            "Favorites" => state_snapshot.favorites.iter().any(|id| id == &app.id),
            "Recent" => state_snapshot.recents.iter().any(|id| id == &app.id),
            category => app.category == category,
        })
        .filter_map(|app| app_search_score(&app.name, &query).map(|score| (score, app)))
        .collect();

    matches.sort_by(|(score_a, app_a), (score_b, app_b)| {
        score_a.cmp(score_b).then_with(|| {
            if category == "Recent" && query.is_empty() {
                let position_a = state_snapshot
                    .recents
                    .iter()
                    .position(|id| id == &app_a.id)
                    .unwrap_or(usize::MAX);
                let position_b = state_snapshot
                    .recents
                    .iter()
                    .position(|id| id == &app_b.id)
                    .unwrap_or(usize::MAX);
                position_a.cmp(&position_b)
            } else {
                app_a.name.to_lowercase().cmp(&app_b.name.to_lowercase())
            }
        })
    });
    drop(state_snapshot);

    for (index, (_, app)) in matches.iter().enumerate() {
        let button = app_button(app, window, state);
        if index == 0 && !query.is_empty() {
            button.add_css_class("launcher-best-match");
        }
        flow.insert(&button, -1);
    }

    if matches.is_empty() {
        if query.is_empty() && category == "Favorites" {
            empty_state.set(
                StateKind::Empty,
                "No favorite applications yet",
                "Right-click an application and add it to Favorites.",
            );
        } else if query.is_empty() && category == "Recent" {
            empty_state.set(
                StateKind::Empty,
                "No recently launched applications",
                "Applications you launch will appear here.",
            );
        } else {
            empty_state.set(
                StateKind::Empty,
                "No applications found",
                "Try another search or category.",
            );
        }
        results.set_visible_child_name("empty");
    } else {
        results.set_visible_child_name("apps");
    }
}

fn build_ui(app: &adw::Application) {
    install_focaldesk_theme();
    let window = adw::ApplicationWindow::new(app);
    window.add_css_class("focaldesk-app");
    window.set_title(Some("Launcher"));
    window.set_default_size(720, 620);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("launcher-root");
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

    let quick = gtk::FlowBox::new();
    quick.set_selection_mode(gtk::SelectionMode::None);
    quick.set_homogeneous(true);
    quick.set_min_children_per_line(1);
    quick.set_max_children_per_line(3);
    quick.set_row_spacing(8);
    quick.set_column_spacing(8);
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
        button.add_css_class("launcher-quick");
        button.set_icon_name(icon);
        button.set_height_request(42);
        button.set_hexpand(true);
        let window = window.clone();
        button.connect_clicked(move |_| {
            spawn(&resolve_command());
            window.close();
        });
        quick.insert(&button, -1);
    }
    root.append(&quick);

    let categories = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let selected_category = Rc::new(RefCell::new("All"));
    let category_scroll = gtk::ScrolledWindow::new();
    category_scroll.add_css_class("launcher-category-scroll");
    category_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    category_scroll.set_propagate_natural_height(true);
    category_scroll.set_child(Some(&categories));
    root.append(&category_scroll);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(2);
    flow.set_max_children_per_line(8);
    flow.set_row_spacing(8);
    flow.set_column_spacing(8);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&flow));

    let empty_state = StateView::new(
        StateKind::Empty,
        "No applications found",
        "Try another search or category.",
    );

    let results = gtk::Stack::new();
    results.set_vexpand(true);
    results.set_transition_type(gtk::StackTransitionType::Crossfade);
    results.add_named(&scroll, Some("apps"));
    results.add_named(&empty_state.widget(), Some("empty"));
    root.append(&results);

    let apps = Rc::new(installed_apps());
    let launcher_state = Rc::new(RefCell::new(load_launcher_state()));
    for category in CATEGORIES {
        let button = gtk::ToggleButton::with_label(category);
        button.add_css_class("launcher-category");
        button.set_active(category == "All");
        if let Some(previous) = categories.last_child().and_downcast::<gtk::ToggleButton>() {
            button.set_group(Some(&previous));
        }
        let apps = apps.clone();
        let flow = flow.clone();
        let search = search.clone();
        let window = window.clone();
        let selected_category = selected_category.clone();
        let results = results.clone();
        let empty_state = empty_state.clone();
        let launcher_state = launcher_state.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                *selected_category.borrow_mut() = category;
                render_apps(
                    &flow,
                    &results,
                    &empty_state,
                    &apps,
                    category,
                    search.text().as_str(),
                    &window,
                    &launcher_state,
                );
            }
        });
        categories.append(&button);
    }

    let apps_for_search = apps.clone();
    let flow_for_search = flow.clone();
    let window_for_search = window.clone();
    let results_for_search = results.clone();
    let empty_state_for_search = empty_state.clone();
    let state_for_search = launcher_state.clone();
    search.connect_search_changed(move |entry| {
        render_apps(
            &flow_for_search,
            &results_for_search,
            &empty_state_for_search,
            &apps_for_search,
            *selected_category.borrow(),
            entry.text().as_str(),
            &window_for_search,
            &state_for_search,
        );
    });
    render_apps(
        &flow,
        &results,
        &empty_state,
        &apps,
        "All",
        "",
        &window,
        &launcher_state,
    );

    let search_keys = gtk::EventControllerKey::new();
    let flow_for_keys = flow.clone();
    search_keys.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Return | gdk::Key::KP_Enter => {
            if let Some(button) = flow_for_keys
                .child_at_index(0)
                .and_then(|child| child.child())
                .and_downcast::<gtk::Button>()
            {
                button.emit_clicked();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
        gdk::Key::Down => {
            if let Some(child) = flow_for_keys
                .child_at_index(0)
                .and_then(|child| child.child())
            {
                child.grab_focus();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
        _ => glib::Propagation::Proceed,
    });
    search.add_controller(search_keys);

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
    search.grab_focus();
}

fn install_focaldesk_theme() {
    let provider = gtk::CssProvider::new();
    let initial = active_theme_snapshot();
    apply_theme_snapshot(&provider, &initial);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

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

fn active_theme_snapshot() -> (String, GtkAppThemeOptions) {
    let config = load_config();
    let settings = load_settings();
    (
        config.appearance.theme,
        GtkAppThemeOptions {
            font_scale: config.appearance.font_scale,
            animations: settings.appearance.animations,
            high_contrast: settings.appearance.high_contrast,
        },
    )
}

fn apply_theme_snapshot(provider: &gtk::CssProvider, snapshot: &(String, GtkAppThemeOptions)) {
    let theme = theme_by_name(&snapshot.0);
    adw::StyleManager::default().set_color_scheme(if gtk_app_prefers_dark(&theme) {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    provider.load_from_string(&gtk_app_css(&theme, snapshot.1));
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(snapshot.1.animations);
    }
}

fn main() {
    init_default_logging();
    let app = adw::Application::new(Some("com.focaldesk.Launcher"), Default::default());
    app.connect_activate(build_ui);
    app.run();
}

#[cfg(test)]
mod tests {
    use super::{app_search_score, remember_recent, toggle_favorite, LauncherState};

    #[test]
    fn search_prioritizes_exact_prefix_and_word_prefix_matches() {
        assert_eq!(app_search_score("Files", "files"), Some(0));
        assert_eq!(app_search_score("Firefox", "fire"), Some(1));
        assert_eq!(app_search_score("Visual Studio Code", "studio"), Some(2));
        assert_eq!(app_search_score("Image Preview", "view"), Some(3));
        assert_eq!(app_search_score("Calculator", "files"), None);
    }

    #[test]
    fn recent_apps_move_to_the_front_without_duplicates() {
        let mut state = LauncherState::default();
        remember_recent(&mut state, "one.desktop");
        remember_recent(&mut state, "two.desktop");
        remember_recent(&mut state, "one.desktop");
        assert_eq!(state.recents, ["one.desktop", "two.desktop"]);
    }

    #[test]
    fn favorites_toggle_cleanly() {
        let mut state = LauncherState::default();
        assert!(toggle_favorite(&mut state, "one.desktop"));
        assert_eq!(state.favorites, ["one.desktop"]);
        assert!(!toggle_favorite(&mut state, "one.desktop"));
        assert!(state.favorites.is_empty());
    }
}
