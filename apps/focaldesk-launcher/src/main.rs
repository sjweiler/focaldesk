use adw::prelude::*;
use focaldesk_config::load_config;
use focaldesk_gtk::{StateKind, StateView, ToastOverlay};
use focaldesk_launcher_state::{
    load_launcher_state, remember_recent_app, remove_file_favorite, toggle_app_favorite,
    LauncherState,
};
use focaldesk_logging::init_default_logging;
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{gtk_app_css, gtk_app_prefers_dark, theme_by_name, GtkAppThemeOptions};
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
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

#[derive(Clone)]
struct FavoriteFile {
    uri: String,
    name: String,
    file: gio::File,
    icon: gio::Icon,
    is_dir: bool,
    available: bool,
}

#[derive(Clone)]
struct LauncherView {
    flow: gtk::FlowBox,
    results: gtk::Stack,
    empty_state: StateView,
    apps: Rc<Vec<LauncherApp>>,
    window: adw::ApplicationWindow,
    state: Rc<RefCell<LauncherState>>,
    toasts: ToastOverlay,
    animations: Rc<RefCell<Vec<adw::TimedAnimation>>>,
    render_generation: Rc<Cell<u64>>,
}

type QuickLaunchEntry = (&'static str, &'static str, Box<dyn Fn() -> String>);

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

fn favorite_file(uri: &str) -> FavoriteFile {
    let file = gio::File::for_uri(uri);
    let info = file
        .query_info(
            "standard::display-name,standard::icon,standard::type",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok();
    let name = info
        .as_ref()
        .map(|info| info.display_name().to_string())
        .or_else(|| {
            file.basename()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| uri.to_string());
    let icon = info
        .as_ref()
        .and_then(|info| info.icon())
        .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic-symbolic").upcast());
    let is_dir = info
        .as_ref()
        .is_some_and(|info| info.file_type() == gio::FileType::Directory);
    let available = info.is_some();

    FavoriteFile {
        uri: uri.to_string(),
        name,
        file,
        icon,
        is_dir,
        available,
    }
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
    toasts: &ToastOverlay,
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
    let window_for_launch = window.clone();
    let toasts_for_launch = toasts.clone();
    button.connect_clicked(move |_| {
        let context =
            gtk::prelude::WidgetExt::display(&window_for_launch).app_launch_context();
        apply_launch_environment(&context, app_targets_exe(&info));
        if let Err(err) = info.launch(&[], Some(&context)) {
            warn!(app = %info.display_name(), %err, "focaldesk-launcher: failed to launch desktop app");
            toasts_for_launch.show(
                StateKind::Error,
                &format!("Could not launch {}: {err}", info.display_name()),
            );
        } else {
            if let Err(err) = remember_recent_app(&app_id) {
                warn!(%err, "focaldesk-launcher: failed to update recent applications");
            }
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
    let button_for_favorite = button.clone();
    let popover_for_action = popover.clone();
    let toasts_for_favorite = toasts.clone();
    favorite_action.connect_clicked(move |action| {
        match toggle_app_favorite(&app_id) {
            Ok(favorite) => {
                if favorite {
                    button_for_favorite.add_css_class("launcher-favorite");
                    action.set_label("Remove from Favorites");
                } else {
                    button_for_favorite.remove_css_class("launcher-favorite");
                    action.set_label("Add to Favorites");
                }
            }
            Err(err) => {
                warn!(%err, "focaldesk-launcher: failed to update application favorite");
                toasts_for_favorite.show(
                    StateKind::Error,
                    &format!("Could not update Favorites: {err}"),
                );
            }
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

fn favorite_file_button(
    favorite: &FavoriteFile,
    window: &adw::ApplicationWindow,
    toasts: &ToastOverlay,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("launcher-app-tile");
    button.add_css_class("launcher-favorite");
    button.set_size_request(132, 92);
    button.set_hexpand(true);
    if favorite.available {
        button.set_tooltip_text(Some(&format!(
            "{} · Right-click to remove from Favorites",
            favorite.name
        )));
    } else {
        button.add_css_class("launcher-unavailable");
        button.set_tooltip_text(Some(&format!(
            "{} is unavailable · Right-click to remove from Favorites",
            favorite.name
        )));
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_valign(gtk::Align::Center);
    let image = gtk::Image::from_gicon(&favorite.icon);
    image.set_pixel_size(40);
    let label = gtk::Label::new(Some(&favorite.name));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(16);
    content.append(&image);
    content.append(&label);
    if !favorite.available {
        let unavailable = gtk::Label::new(Some("Unavailable"));
        unavailable.add_css_class("dim-label");
        unavailable.add_css_class("caption");
        content.append(&unavailable);
    }
    button.set_child(Some(&content));

    let file = favorite.file.clone();
    let is_dir = favorite.is_dir;
    let available = favorite.available;
    let favorite_name = favorite.name.clone();
    let window_for_launch = window.clone();
    let toasts_for_launch = toasts.clone();
    button.connect_clicked(move |_| {
        if !available {
            toasts_for_launch.show(
                StateKind::Error,
                &format!("Favorite is unavailable: {favorite_name}"),
            );
            return;
        }
        let result = if is_dir {
            if let Some(path) = file.path() {
                Command::new(sibling_binary("focaldesk-files"))
                    .arg(path)
                    .spawn()
                    .map(|_| ())
                    .map_err(|err| glib::Error::new(gio::IOErrorEnum::Failed, &err.to_string()))
            } else {
                let context =
                    gtk::prelude::WidgetExt::display(&window_for_launch).app_launch_context();
                apply_launch_environment(&context, false);
                gio::AppInfo::launch_default_for_uri(&file.uri(), Some(&context))
            }
        } else {
            let context = gtk::prelude::WidgetExt::display(&window_for_launch).app_launch_context();
            apply_launch_environment(&context, file_requires_x11(&file));
            gio::AppInfo::launch_default_for_uri(&file.uri(), Some(&context))
        };

        match result {
            Ok(()) => window_for_launch.close(),
            Err(err) => {
                warn!(uri = %file.uri(), %err, "focaldesk-launcher: failed to open favorite file");
                toasts_for_launch.show(
                    StateKind::Error,
                    &format!("Could not open {favorite_name}: {err}"),
                );
            }
        }
    });

    let popover = gtk::Popover::new();
    popover.set_has_arrow(true);
    popover.set_parent(&button);
    let remove_action = gtk::Button::with_label("Remove from Favorites");
    remove_action.add_css_class("flat");
    popover.set_child(Some(&remove_action));

    let uri = favorite.uri.clone();
    let button_for_remove = button.clone();
    let popover_for_remove = popover.clone();
    let toasts_for_remove = toasts.clone();
    remove_action.connect_clicked(move |_| {
        match remove_file_favorite(&uri) {
            Ok(_) => {
                button_for_remove.set_visible(false);
            }
            Err(err) => {
                warn!(%err, "focaldesk-launcher: failed to remove file favorite");
                toasts_for_remove.show(
                    StateKind::Error,
                    &format!("Could not update Favorites: {err}"),
                );
            }
        }
        popover_for_remove.popdown();
    });

    let secondary_click = gtk::GestureClick::new();
    secondary_click.set_button(gdk::BUTTON_SECONDARY);
    secondary_click.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        popover.popup();
    });
    button.add_controller(secondary_click);
    button
}

fn file_requires_x11(file: &gio::File) -> bool {
    file.path().as_deref().is_some_and(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    })
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

impl LauncherView {
    fn render(&self, category: &str, query: &str) {
        self.animations.borrow_mut().clear();
        let generation = self.render_generation.get().wrapping_add(1);
        self.render_generation.set(generation);
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }

        let query = query.trim().to_lowercase();
        let state_snapshot = self.state.borrow();
        let favorite_file_uris =
            if category == "Favorites" || (category == "All" && !query.is_empty()) {
                state_snapshot.file_favorites.clone()
            } else {
                Vec::new()
            };
        let mut matches: Vec<_> = self
            .apps
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

        let mut file_matches: Vec<_> = favorite_file_uris
            .iter()
            .map(|uri| favorite_file(uri))
            .filter_map(|file| app_search_score(&file.name, &query).map(|score| (score, file)))
            .collect();
        file_matches.sort_by(|(score_a, file_a), (score_b, file_b)| {
            score_a
                .cmp(score_b)
                .then_with(|| file_a.name.to_lowercase().cmp(&file_b.name.to_lowercase()))
        });
        let animate_results = load_settings().appearance.animations;

        for (index, (_, app)) in matches.iter().enumerate() {
            let button = app_button(app, &self.window, &self.state, &self.toasts);
            if index == 0 && !query.is_empty() {
                button.add_css_class("launcher-best-match");
            }
            self.flow.insert(&button, -1);
            self.animate_result(&button, index, generation, animate_results);
        }

        let app_match_count = matches.len();
        for (index, (_, file)) in file_matches.iter().enumerate() {
            let button = favorite_file_button(file, &self.window, &self.toasts);
            if app_match_count + index == 0 && !query.is_empty() {
                button.add_css_class("launcher-best-match");
            }
            self.flow.insert(&button, -1);
            self.animate_result(
                &button,
                app_match_count + index,
                generation,
                animate_results,
            );
        }

        if matches.is_empty() && file_matches.is_empty() {
            if query.is_empty() && category == "Favorites" {
                self.empty_state.set(
                    StateKind::Empty,
                    "No favorites yet",
                    "Favorite an application here or a file in Files.",
                );
            } else if query.is_empty() && category == "Recent" {
                self.empty_state.set(
                    StateKind::Empty,
                    "No recently launched applications",
                    "Applications you launch will appear here.",
                );
            } else {
                self.empty_state.set(
                    StateKind::Empty,
                    "No applications found",
                    "Try another search or category.",
                );
            }
            self.results.set_visible_child_name("empty");
        } else {
            self.results.set_visible_child_name("apps");
        }
    }

    fn animate_result(&self, button: &gtk::Button, index: usize, generation: u64, enabled: bool) {
        if !enabled {
            return;
        }

        button.set_opacity(0.0);
        let weak_button = button.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            if let Some(button) = weak_button.upgrade() {
                button.set_opacity(value);
            }
        });
        let animation = adw::TimedAnimation::new(button, 0.0, 1.0, 180, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        self.animations.borrow_mut().push(animation.clone());

        let render_generation = self.render_generation.clone();
        glib::timeout_add_local_once(
            Duration::from_millis((index.min(8) as u64) * 24),
            move || {
                if render_generation.get() == generation {
                    animation.play();
                }
            },
        );
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
    root.add_css_class("shell-surface");
    root.set_margin_top(10);
    root.set_margin_bottom(14);
    root.set_margin_start(14);
    root.set_margin_end(14);

    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&root));
    let toasts = ToastOverlay::new(&toolbar_view);
    window.set_content(Some(&toasts.widget()));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search applications and favorites"));
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
    let entries: [QuickLaunchEntry; 3] = [
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
    results.set_transition_duration(180);
    results.add_named(&scroll, Some("apps"));
    results.add_named(&empty_state.widget(), Some("empty"));
    root.append(&results);

    let apps = Rc::new(installed_apps());
    let launcher_state = Rc::new(RefCell::new(load_launcher_state()));
    let view = LauncherView {
        flow: flow.clone(),
        results: results.clone(),
        empty_state: empty_state.clone(),
        apps: apps.clone(),
        window: window.clone(),
        state: launcher_state.clone(),
        toasts,
        animations: Rc::new(RefCell::new(Vec::new())),
        render_generation: Rc::new(Cell::new(0)),
    };
    for category in CATEGORIES {
        let button = gtk::ToggleButton::with_label(category);
        button.add_css_class("launcher-category");
        button.set_active(category == "All");
        if let Some(previous) = categories.last_child().and_downcast::<gtk::ToggleButton>() {
            button.set_group(Some(&previous));
        }
        let search = search.clone();
        let selected_category = selected_category.clone();
        let view = view.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                *selected_category.borrow_mut() = category;
                view.render(category, search.text().as_str());
            }
        });
        categories.append(&button);
    }

    let view_for_search = view.clone();
    let selected_category_for_search = selected_category.clone();
    search.connect_search_changed(move |entry| {
        view_for_search.render(
            *selected_category_for_search.borrow(),
            entry.text().as_str(),
        );
    });
    view.render("All", "");

    let view_for_state_changes = view.clone();
    let state_for_changes = launcher_state.clone();
    let category_for_changes = selected_category.clone();
    let search_for_changes = search.clone();
    glib::timeout_add_local(Duration::from_millis(350), move || {
        let next = load_launcher_state();
        if next != *state_for_changes.borrow() {
            *state_for_changes.borrow_mut() = next;
            view_for_state_changes.render(
                *category_for_changes.borrow(),
                search_for_changes.text().as_str(),
            );
        }
        glib::ControlFlow::Continue
    });

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
    use super::{app_search_score, file_requires_x11};
    use gtk::gio;

    #[test]
    fn search_prioritizes_exact_prefix_and_word_prefix_matches() {
        assert_eq!(app_search_score("Files", "files"), Some(0));
        assert_eq!(app_search_score("Firefox", "fire"), Some(1));
        assert_eq!(app_search_score("Visual Studio Code", "studio"), Some(2));
        assert_eq!(app_search_score("Image Preview", "view"), Some(3));
        assert_eq!(app_search_score("Calculator", "files"), None);
    }

    #[test]
    fn windows_executable_favorites_force_xwayland() {
        assert!(file_requires_x11(&gio::File::for_path("/games/Game.EXE")));
        assert!(!file_requires_x11(&gio::File::for_path(
            "/games/readme.txt"
        )));
        assert!(!file_requires_x11(&gio::File::for_uri(
            "smb://server/Game.exe"
        )));
    }
}
