use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, Clone)]
struct FileItem {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: String,
}

#[derive(Clone)]
struct FileManager {
    window: adw::ApplicationWindow,
    path_entry: gtk::Entry,
    list: gtk::ListBox,
    status: gtk::Label,
    hidden_toggle: gtk::ToggleButton,
    places: gtk::StringList,
    current_dir: Rc<RefCell<PathBuf>>,
    entries: Rc<RefCell<Vec<FileItem>>>,
    back_stack: Rc<RefCell<Vec<PathBuf>>>,
    forward_stack: Rc<RefCell<Vec<PathBuf>>>,
}

fn main() {
    let app = adw::Application::new(
        Some("com.flowstate.Files"),
        gio::ApplicationFlags::HANDLES_OPEN,
    );
    app.connect_activate(|app| build_ui(app, None));
    app.connect_open(|app, files, _| {
        let initial_path = files.first().and_then(|file| file.path());
        build_ui(app, initial_path);
    });
    app.run();
}

fn build_ui(app: &adw::Application, initial_path: Option<PathBuf>) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("FlowState Files"));
    window.set_default_size(1040, 680);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    toolbar.add_top_bar(&header);

    let back_button = icon_button("go-previous-symbolic", "Back");
    let forward_button = icon_button("go-next-symbolic", "Forward");
    let up_button = icon_button("go-up-symbolic", "Up");
    let home_button = icon_button("go-home-symbolic", "Home");
    let refresh_button = icon_button("view-refresh-symbolic", "Refresh");
    let new_folder_button = icon_button("folder-new-symbolic", "New Folder");
    let trash_button = icon_button("user-trash-symbolic", "Move to Trash");

    header.pack_start(&back_button);
    header.pack_start(&forward_button);
    header.pack_start(&up_button);
    header.pack_start(&home_button);

    let path_entry = gtk::Entry::new();
    path_entry.set_hexpand(true);
    path_entry.set_placeholder_text(Some("Path"));
    path_entry.set_primary_icon_name(Some("folder-symbolic"));
    header.set_title_widget(Some(&path_entry));

    let hidden_toggle = gtk::ToggleButton::new();
    hidden_toggle.set_icon_name("view-hidden-symbolic");
    hidden_toggle.set_tooltip_text(Some("Show Hidden Files"));
    header.pack_end(&hidden_toggle);
    header.pack_end(&refresh_button);
    header.pack_end(&trash_button);
    header.pack_end(&new_folder_button);

    let split = adw::NavigationSplitView::new();
    split.set_min_sidebar_width(180.0);
    split.set_max_sidebar_width(240.0);

    let places = gtk::StringList::new(&[]);
    let selection = gtk::SingleSelection::new(Some(places.clone()));
    let places_view = gtk::ListView::new(Some(selection.clone()), Some(place_factory()));
    places_view.add_css_class("navigation-sidebar");

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);
    sidebar_box.append(&places_view);
    split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar_box, "Places")));

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_vexpand(true);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&list));

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.set_margin_top(8);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&column_header());
    content.append(&scroller);
    content.append(&status);
    split.set_content(Some(&adw::NavigationPage::new(&content, "Files")));

    toolbar.set_content(Some(&split));
    window.set_content(Some(&toolbar));

    let manager = FileManager {
        window,
        path_entry,
        list,
        status,
        hidden_toggle,
        places,
        current_dir: Rc::new(RefCell::new(home_dir())),
        entries: Rc::new(RefCell::new(Vec::new())),
        back_stack: Rc::new(RefCell::new(Vec::new())),
        forward_stack: Rc::new(RefCell::new(Vec::new())),
    };

    manager.install_css();
    manager.load_places();
    manager.connect_actions(
        back_button,
        forward_button,
        up_button,
        home_button,
        refresh_button,
        new_folder_button,
        trash_button,
        places_view,
    );
    manager.open_initial_path(initial_path.unwrap_or_else(home_dir));
    manager.window.present();
}

impl FileManager {
    fn connect_actions(
        &self,
        back_button: gtk::Button,
        forward_button: gtk::Button,
        up_button: gtk::Button,
        home_button: gtk::Button,
        refresh_button: gtk::Button,
        new_folder_button: gtk::Button,
        trash_button: gtk::Button,
        places_view: gtk::ListView,
    ) {
        let this = self.clone();
        self.path_entry.connect_activate(move |entry| {
            this.open_dir(PathBuf::from(entry.text().as_str()), true);
        });

        let this = self.clone();
        self.list.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }

            let Some(item) = this.entries.borrow().get(index as usize).cloned() else {
                return;
            };

            if item.is_dir {
                this.open_dir(item.path, true);
            } else if let Err(err) = open_file(&item.path) {
                this.set_status(&format!("Could not open {}: {err}", item.name));
            }
        });

        let this = self.clone();
        back_button.connect_clicked(move |_| {
            let Some(previous) = this.back_stack.borrow_mut().pop() else {
                return;
            };
            this.forward_stack
                .borrow_mut()
                .push(this.current_dir.borrow().clone());
            this.open_dir(previous, false);
        });

        let this = self.clone();
        forward_button.connect_clicked(move |_| {
            let Some(next) = this.forward_stack.borrow_mut().pop() else {
                return;
            };
            this.back_stack
                .borrow_mut()
                .push(this.current_dir.borrow().clone());
            this.open_dir(next, false);
        });

        let this = self.clone();
        up_button.connect_clicked(move |_| {
            let parent = this.current_dir.borrow().parent().map(Path::to_path_buf);
            if let Some(parent) = parent {
                this.open_dir(parent, true);
            }
        });

        let this = self.clone();
        home_button.connect_clicked(move |_| this.open_dir(home_dir(), true));

        let this = self.clone();
        refresh_button.connect_clicked(move |_| this.reload());

        let this = self.clone();
        self.hidden_toggle.connect_toggled(move |_| this.reload());

        let this = self.clone();
        new_folder_button.connect_clicked(move |_| this.show_new_folder_dialog());

        let this = self.clone();
        trash_button.connect_clicked(move |_| this.trash_selected());

        let this = self.clone();
        places_view.connect_activate(move |_, position| {
            if let Some(path) = this.place_path(position) {
                this.open_dir(path, true);
            }
        });
    }

    fn install_css(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(
            "
            .file-row {
                padding: 8px 10px;
            }
            .file-name {
                font-weight: 500;
            }
            .column-header {
                padding: 0 10px 4px 46px;
            }
            ",
        );

        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    fn load_places(&self) {
        let mut paths = vec![
            ("Home", home_dir()),
            ("Desktop", home_dir().join("Desktop")),
            ("Documents", home_dir().join("Documents")),
            ("Downloads", home_dir().join("Downloads")),
            ("Pictures", home_dir().join("Pictures")),
            ("Music", home_dir().join("Music")),
            ("Videos", home_dir().join("Videos")),
            ("File System", PathBuf::from("/")),
        ];

        paths.retain(|(_, path)| path.exists());
        for (name, path) in paths {
            self.places
                .append(&format!("{name}\n{}", path.to_string_lossy()));
        }
    }

    fn place_path(&self, position: u32) -> Option<PathBuf> {
        self.places.string(position).and_then(|text| {
            text.lines()
                .nth(1)
                .map(|path| PathBuf::from(path.to_string()))
        })
    }

    fn open_dir(&self, path: PathBuf, remember: bool) {
        let path = normalize_path(&path);
        if !path.is_dir() {
            self.set_status(&format!("Not a folder: {}", path.display()));
            self.path_entry
                .set_text(&self.current_dir.borrow().to_string_lossy());
            return;
        }

        match read_dir_items(&path, self.hidden_toggle.is_active()) {
            Ok(items) => {
                if remember && *self.current_dir.borrow() != path {
                    self.back_stack
                        .borrow_mut()
                        .push(self.current_dir.borrow().clone());
                    self.forward_stack.borrow_mut().clear();
                }

                *self.current_dir.borrow_mut() = path.clone();
                *self.entries.borrow_mut() = items;
                self.path_entry.set_text(&path.to_string_lossy());
                self.render_entries();
            }
            Err(err) => {
                self.set_status(&format!("Could not read {}: {err}", path.display()));
                self.path_entry
                    .set_text(&self.current_dir.borrow().to_string_lossy());
            }
        }
    }

    fn open_initial_path(&self, path: PathBuf) {
        let path = normalize_path(&path);
        let dir = if path.is_dir() {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(home_dir)
        };

        self.open_dir(dir, false);
    }

    fn reload(&self) {
        self.open_dir(self.current_dir.borrow().clone(), false);
    }

    fn render_entries(&self) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }

        for item in self.entries.borrow().iter() {
            self.list.append(&file_row(item));
        }

        let entries = self.entries.borrow();
        let folders = entries.iter().filter(|item| item.is_dir).count();
        let files = entries.len().saturating_sub(folders);
        self.set_status(&format!(
            "{} folder{}, {} file{}",
            folders,
            plural(folders),
            files,
            plural(files)
        ));
    }

    fn show_new_folder_dialog(&self) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("New Folder")
            .default_width(360)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let label = gtk::Label::new(Some("Folder name"));
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Folder name"));

        let cancel = gtk::Button::with_label("Cancel");
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&create);

        content.append(&label);
        content.append(&entry);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());

        let this = self.clone();
        let dialog_for_create = dialog.clone();
        create.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if !name.is_empty() && !name.contains('/') {
                let path = this.current_dir.borrow().join(name);
                match std::fs::create_dir(&path) {
                    Ok(()) => this.reload(),
                    Err(err) => this.set_status(&format!("Could not create folder: {err}")),
                }
            } else {
                this.set_status("Folder names cannot be empty or contain '/'.");
            }
            dialog_for_create.close();
        });

        dialog.present();
    }

    fn trash_selected(&self) {
        let Some(row) = self.list.selected_row() else {
            self.set_status("Select an item to move to trash.");
            return;
        };

        let index = row.index();
        if index < 0 {
            return;
        }

        let Some(item) = self.entries.borrow().get(index as usize).cloned() else {
            return;
        };

        let file = gio::File::for_path(&item.path);
        match file.trash(gio::Cancellable::NONE) {
            Ok(()) => {
                self.set_status(&format!("Moved {} to trash.", item.name));
                self.reload();
            }
            Err(err) => self.set_status(&format!("Could not move {} to trash: {err}", item.name)),
        }
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }
}

fn read_dir_items(path: &Path, show_hidden: bool) -> Result<Vec<FileItem>, glib::Error> {
    let file = gio::File::for_path(path);
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::size,time::modified",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )?;

    let mut items = Vec::new();
    while let Some(info) = enumerator.next_file(gio::Cancellable::NONE)? {
        let name = info.name().to_string_lossy().into_owned();

        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let is_dir = info.file_type() == gio::FileType::Directory;
        items.push(FileItem {
            path: path.join(&name),
            name,
            is_dir,
            size: info.size().max(0) as u64,
            modified: modified_text(&info),
        });
    }

    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(items)
}

fn file_row(item: &FileItem) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_hexpand(true);

    let icon = gtk::Image::from_icon_name(if item.is_dir {
        "folder-symbolic"
    } else {
        "text-x-generic-symbolic"
    });
    icon.set_pixel_size(22);
    grid.attach(&icon, 0, 0, 1, 1);

    let name = gtk::Label::new(Some(&item.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("file-name");
    grid.attach(&name, 1, 0, 1, 1);

    let size = gtk::Label::new(Some(
        if item.is_dir {
            "Folder".to_string()
        } else {
            format_size(item.size)
        }
        .as_str(),
    ));
    size.set_xalign(1.0);
    size.set_width_chars(12);
    size.add_css_class("dim-label");
    grid.attach(&size, 2, 0, 1, 1);

    let modified = gtk::Label::new(Some(&item.modified));
    modified.set_xalign(1.0);
    modified.set_width_chars(18);
    modified.add_css_class("dim-label");
    grid.attach(&modified, 3, 0, 1, 1);

    row.set_child(Some(&grid));
    row
}

fn column_header() -> gtk::Grid {
    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.add_css_class("column-header");

    let name = gtk::Label::new(Some("Name"));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    grid.attach(&name, 0, 0, 1, 1);

    let size = gtk::Label::new(Some("Size"));
    size.set_xalign(1.0);
    size.set_width_chars(12);
    grid.attach(&size, 1, 0, 1, 1);

    let modified = gtk::Label::new(Some("Modified"));
    modified.set_xalign(1.0);
    modified.set_width_chars(18);
    grid.attach(&modified, 2, 0, 1, 1);

    grid
}

fn place_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_margin_top(8);
        label.set_margin_bottom(8);
        label.set_margin_start(10);
        label.set_margin_end(10);
        item.downcast_ref::<gtk::ListItem>()
            .expect("ListItem")
            .set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let label = item
            .child()
            .and_downcast::<gtk::Label>()
            .expect("Label child");
        let Some(place) = item.item().and_downcast::<gtk::StringObject>() else {
            return;
        };

        let name = place.string();
        label.set_text(name.lines().next().unwrap_or_default());
        label.set_tooltip_text(name.lines().nth(1));
    });
    factory
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn open_file(path: &Path) -> Result<(), glib::Error> {
    if path.extension().is_some_and(|ext| ext == "desktop") {
        return launch_desktop_entry(path).map_err(|err| {
            glib::Error::new(
                gio::IOErrorEnum::Failed,
                &format!("Could not launch desktop entry: {err}"),
            )
        });
    }

    let force_x11 = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    let uri = gio::File::for_path(path).uri();
    if let Some(display) = gtk::gdk::Display::default() {
        let context = display.app_launch_context();
        apply_launch_environment(&context, force_x11);
        gio::AppInfo::launch_default_for_uri(&uri, Some(&context))
    } else {
        let context = gio::AppLaunchContext::new();
        apply_launch_environment(&context, force_x11);
        gio::AppInfo::launch_default_for_uri(&uri, Some(&context))
    }
}

fn launch_desktop_entry(path: &Path) -> io::Result<()> {
    let path_str = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "desktop path is not valid UTF-8",
        )
    })?;

    log_launch(&format!("launch desktop entry {}", path.display()));
    log_launch_environment();

    let app_info = gio::DesktopAppInfo::from_filename(path_str).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "failed to parse desktop entry")
    })?;

    let force_x11 = desktop_entry_targets_exe(&app_info);
    let context = gio::AppLaunchContext::new();
    apply_launch_environment(&context, force_x11);
    if force_x11 {
        log_launch("unset WAYLAND_DISPLAY for .exe desktop entry (XWayland-only launch)");
    }

    app_info.launch(&[], Some(&context)).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("desktop entry launch failed: {err}"),
        )
    })?;

    log_launch(&format!(
        "desktop entry launched via GIO: {}",
        app_info.name()
    ));
    Ok(())
}

const LAUNCH_ENV_KEYS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// When `force_x11` is true, omit `WAYLAND_DISPLAY` so Wine/Proton uses XWayland via `DISPLAY`.
fn apply_launch_environment(context: &impl IsA<gio::AppLaunchContext>, force_x11: bool) {
    for key in LAUNCH_ENV_KEYS {
        if force_x11 && *key == "WAYLAND_DISPLAY" {
            context.unsetenv(key);
            continue;
        }
        if let Some(value) = std::env::var_os(key) {
            context.setenv(key, value);
        } else {
            context.unsetenv(key);
        }
    }
}

fn desktop_entry_targets_exe(app_info: &gio::DesktopAppInfo) -> bool {
    if let Some(exec) = app_info.string("Exec") {
        if exec.to_ascii_lowercase().contains(".exe") {
            return true;
        }
    }
    app_info
        .commandline()
        .and_then(|path| path.to_str().map(str::to_ascii_lowercase))
        .is_some_and(|cmd| cmd.contains(".exe"))
}

fn log_launch(msg: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/flowstate-files.log")
    {
        let _ = writeln!(file, "{msg}");
    }
}

fn log_launch_environment() {
    for key in LAUNCH_ENV_KEYS {
        let value = std::env::var(key).unwrap_or_else(|_| "<unset>".to_string());
        log_launch(&format!("launch env {key}={value}"));
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    if path == Path::new("~") {
        return home_dir();
    }

    if let Ok(stripped) = path.strip_prefix("~") {
        return home_dir().join(stripped);
    }

    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| home_dir())
            .join(path)
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn modified_text(info: &gio::FileInfo) -> String {
    info.modification_date_time()
        .and_then(|date| date.format("%Y-%m-%d %H:%M").ok())
        .map(|text| text.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{size} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}
