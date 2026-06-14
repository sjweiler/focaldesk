use adw::prelude::*;
use focaldesk_logging::flog_info;
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
    file: gio::File,
    path: Option<PathBuf>,
    uri: String,
    is_dir: bool,
    size: u64,
    modified: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarKind {
    Folder,
    Trash,
    Separator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileContextTarget {
    Folder,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    Path(PathBuf),
    Trash,
    Uri(String),
    Separator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Details,
    List,
    Grid,
}

#[derive(Clone)]
struct FileManager {
    window: adw::ApplicationWindow,
    path_entry: gtk::Entry,
    list: gtk::ListBox,
    grid: gtk::FlowBox,
    scroller: gtk::ScrolledWindow,
    column_header: gtk::Grid,
    status: gtk::Label,
    hidden_toggle: gtk::ToggleButton,
    places: gtk::StringList,
    view_mode: Rc<RefCell<ViewMode>>,
    current_location: Rc<RefCell<Location>>,
    entries: Rc<RefCell<Vec<FileItem>>>,
    back_stack: Rc<RefCell<Vec<Location>>>,
    forward_stack: Rc<RefCell<Vec<Location>>>,
}

fn main() {
    let app = adw::Application::new(
        Some("com.focaldesk.Files"),
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
    window.set_title(Some("FocalDesk Files"));
    window.set_default_size(1040, 680);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    toolbar.add_top_bar(&header);

    let back_button = icon_button("go-previous-symbolic", "Back");
    let forward_button = icon_button("go-next-symbolic", "Forward");
    let up_button = icon_button("go-up-symbolic", "Up");
    let home_button = icon_button("go-home-symbolic", "Home");
    let refresh_button = icon_button("view-refresh-symbolic", "Refresh");
    let new_folder_button = icon_button("folder-new-symbolic", "New Folder");
    let trash_button = icon_button("user-trash-symbolic", "Move to Trash");
    let details_view_button = toggle_icon_button("view-list-symbolic", "Details View");
    let list_view_button = toggle_icon_button("view-paged-symbolic", "List View");
    let grid_view_button = toggle_icon_button("view-grid-symbolic", "Grid View");
    let close_button = icon_button("window-close-symbolic", "Close");
    list_view_button.set_group(Some(&details_view_button));
    grid_view_button.set_group(Some(&details_view_button));
    details_view_button.set_active(true);

    header.pack_start(&back_button);
    header.pack_start(&forward_button);
    header.pack_start(&up_button);
    header.pack_start(&home_button);

    let path_entry = gtk::Entry::new();
    path_entry.set_hexpand(true);
    path_entry.set_placeholder_text(Some("Path"));
    path_entry.set_primary_icon_name(Some("folder-symbolic"));
    header.pack_start(&path_entry);
    header.pack_start(&new_folder_button);

    let hidden_toggle = gtk::ToggleButton::new();
    hidden_toggle.set_icon_name("view-hidden-symbolic");
    hidden_toggle.set_tooltip_text(Some("Show Hidden Files"));
    let window_for_close = window.clone();
    close_button.connect_clicked(move |_| window_for_close.close());
    header.pack_end(&close_button);
    header.pack_end(&hidden_toggle);
    header.pack_end(&grid_view_button);
    header.pack_end(&list_view_button);
    header.pack_end(&details_view_button);
    header.pack_end(&refresh_button);
    header.pack_end(&trash_button);

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

    let grid = gtk::FlowBox::new();
    grid.set_selection_mode(gtk::SelectionMode::Single);
    grid.set_valign(gtk::Align::Start);
    grid.set_max_children_per_line(8);
    grid.set_min_children_per_line(2);
    grid.set_row_spacing(8);
    grid.set_column_spacing(8);
    grid.add_css_class("file-grid");
    grid.set_vexpand(true);

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
    let column_header = column_header();
    content.append(&column_header);
    content.append(&scroller);
    content.append(&status);
    split.set_content(Some(&adw::NavigationPage::new(&content, "Files")));

    toolbar.set_content(Some(&split));
    window.set_content(Some(&toolbar));

    let manager = FileManager {
        window,
        path_entry,
        list,
        grid,
        scroller,
        column_header,
        status,
        hidden_toggle,
        places,
        view_mode: Rc::new(RefCell::new(ViewMode::Details)),
        current_location: Rc::new(RefCell::new(Location::Path(home_dir()))),
        entries: Rc::new(RefCell::new(Vec::new())),
        back_stack: Rc::new(RefCell::new(Vec::new())),
        forward_stack: Rc::new(RefCell::new(Vec::new())),
    };

    ensure_standard_user_dirs();
    manager.install_css();
    manager.load_places();
    install_sidebar_actions(manager.window.upcast_ref());
    manager.install_file_context_actions();
    manager.connect_actions(
        back_button,
        forward_button,
        up_button,
        home_button,
        refresh_button,
        new_folder_button,
        trash_button,
        details_view_button,
        list_view_button,
        grid_view_button,
        places_view,
    );
    manager.open_initial_path(initial_path.unwrap_or_else(home_dir));
    manager.window.present();
}

impl FileManager {
    fn install_file_context_actions(&self) {
        let open = gio::SimpleAction::new("file-open", None);
        let this = self.clone();
        open.connect_activate(move |_, _| {
            let this = this.clone();
            glib::idle_add_local_once(move || {
                if let Some(index) = this.selected_index() {
                    this.activate_item(index);
                }
            });
        });
        self.window.add_action(&open);

        let cut = gio::SimpleAction::new("file-cut", None);
        cut.connect_activate(|_, _| {
            flog_info!("Cut file item");
        });
        self.window.add_action(&cut);

        let copy = gio::SimpleAction::new("file-copy", None);
        copy.connect_activate(|_, _| {
            flog_info!("Copy file item");
        });
        self.window.add_action(&copy);

        let move_to = gio::SimpleAction::new("file-move-to", None);
        move_to.connect_activate(|_, _| {
            flog_info!("Move file item to...");
        });
        self.window.add_action(&move_to);

        let copy_to = gio::SimpleAction::new("file-copy-to", None);
        copy_to.connect_activate(|_, _| {
            flog_info!("Copy file item to...");
        });
        self.window.add_action(&copy_to);

        let rename = gio::SimpleAction::new("file-rename", None);
        rename.connect_activate(|_, _| {
            flog_info!("Rename file item");
        });
        self.window.add_action(&rename);

        let paste_into = gio::SimpleAction::new("file-paste-into-folder", None);
        paste_into.connect_activate(|_, _| {
            flog_info!("Paste into folder");
        });
        self.window.add_action(&paste_into);

        let compress = gio::SimpleAction::new("file-compress", None);
        compress.connect_activate(|_, _| {
            flog_info!("Compress file item");
        });
        self.window.add_action(&compress);

        let move_to_trash = gio::SimpleAction::new("file-move-to-trash", None);
        let this = self.clone();
        move_to_trash.connect_activate(move |_, _| {
            let this = this.clone();
            glib::idle_add_local_once(move || this.trash_selected());
        });
        self.window.add_action(&move_to_trash);

        let properties = gio::SimpleAction::new("file-properties", None);
        properties.connect_activate(|_, _| {
            flog_info!("Show file properties");
        });
        self.window.add_action(&properties);
    }

    fn connect_actions(
        &self,
        back_button: gtk::Button,
        forward_button: gtk::Button,
        up_button: gtk::Button,
        home_button: gtk::Button,
        refresh_button: gtk::Button,
        new_folder_button: gtk::Button,
        trash_button: gtk::Button,
        details_view_button: gtk::ToggleButton,
        list_view_button: gtk::ToggleButton,
        grid_view_button: gtk::ToggleButton,
        places_view: gtk::ListView,
    ) {
        let this = self.clone();
        self.path_entry.connect_activate(move |entry| {
            this.open_location(location_from_entry(entry.text().as_str()), true);
        });

        let this = self.clone();
        self.list.connect_selected_rows_changed(move |_| {
            this.show_selected_path();
        });

        let this = self.clone();
        self.grid.connect_selected_children_changed(move |_| {
            this.show_selected_path();
        });

        self.install_drop_target(&self.list);
        self.install_drop_target(&self.grid);

        let this = self.clone();
        self.list.connect_row_activated(move |_, row| {
            this.activate_item(row.index());
        });

        let this = self.clone();
        self.grid.connect_child_activated(move |_, child| {
            this.activate_item(child.index());
        });

        let this = self.clone();
        back_button.connect_clicked(move |_| {
            let Some(previous) = this.back_stack.borrow_mut().pop() else {
                return;
            };
            this.forward_stack
                .borrow_mut()
                .push(this.current_location.borrow().clone());
            this.open_location(previous, false);
        });

        let this = self.clone();
        forward_button.connect_clicked(move |_| {
            let Some(next) = this.forward_stack.borrow_mut().pop() else {
                return;
            };
            this.back_stack
                .borrow_mut()
                .push(this.current_location.borrow().clone());
            this.open_location(next, false);
        });

        let this = self.clone();
        up_button.connect_clicked(move |_| {
            if let Some(parent) = this.parent_dir() {
                this.open_location(Location::Path(parent), true);
            }
        });

        let this = self.clone();
        home_button.connect_clicked(move |_| this.open_location(Location::Path(home_dir()), true));

        let this = self.clone();
        refresh_button.connect_clicked(move |_| this.reload());

        let this = self.clone();
        self.hidden_toggle.connect_toggled(move |_| this.reload());

        let this = self.clone();
        new_folder_button.connect_clicked(move |_| this.show_new_folder_dialog());

        let this = self.clone();
        trash_button.connect_clicked(move |_| this.trash_selected());

        let this = self.clone();
        details_view_button.connect_toggled(move |button| {
            if button.is_active() {
                this.set_view_mode(ViewMode::Details);
            }
        });

        let this = self.clone();
        list_view_button.connect_toggled(move |button| {
            if button.is_active() {
                this.set_view_mode(ViewMode::List);
            }
        });

        let this = self.clone();
        grid_view_button.connect_toggled(move |button| {
            if button.is_active() {
                this.set_view_mode(ViewMode::Grid);
            }
        });

        let this = self.clone();
        places_view.connect_activate(move |_, position| {
            if let Some(location) = this.place_location(position) {
                this.open_location(location, true);
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
            .file-list-row {
                padding: 10px;
            }
            .file-grid {
                padding: 4px;
            }
            .file-grid-child {
                border-radius: 8px;
                padding: 8px;
            }
            .file-grid-tile {
                min-width: 112px;
            }
            .file-grid-name {
                font-weight: 500;
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
            ("Trash", PathBuf::from("trash:///")),
            ("----------", PathBuf::from("-")),
            ("Desktop", home_dir().join("Desktop")),
            ("Music", home_dir().join("Music")),
            ("Pictures", home_dir().join("Pictures")),
            ("Videos", home_dir().join("Videos")),
            ("Downloads", home_dir().join("Downloads")),
        ];

        paths.retain(|(_, path)| {
            path == Path::new("trash:///") || path == Path::new("-") || path.exists()
        });
        for (name, path) in paths {
            self.places
                .append(&format!("{name}\n{}", path.to_string_lossy()));
        }
    }

    fn place_location(&self, position: u32) -> Option<Location> {
        self.places.string(position).and_then(|text| {
            text.lines().nth(1).map(|path| {
                if path == "trash:///" {
                    Location::Trash
                } else if path == "-" {
                    Location::Separator
                } else {
                    Location::Path(PathBuf::from(path))
                }
            })
        })
    }

    fn open_location(&self, location: Location, remember: bool) {
        let location = normalize_location(location);
        if location == Location::Separator {
            return;
        }

        if let Location::Path(path) = &location {
            if !path.is_dir() {
                self.set_status(&format!("Not a folder: {}", path.display()));
                self.path_entry
                    .set_text(&self.current_location.borrow().display_text());
                return;
            }
        }

        match read_location_items(&location, self.hidden_toggle.is_active()) {
            Ok(items) => {
                if remember && *self.current_location.borrow() != location {
                    self.back_stack
                        .borrow_mut()
                        .push(self.current_location.borrow().clone());
                    self.forward_stack.borrow_mut().clear();
                }

                *self.current_location.borrow_mut() = location.clone();
                *self.entries.borrow_mut() = items;
                self.path_entry.set_text(&location.display_text());
                self.render_entries();
            }
            Err(err) => {
                self.set_status(&format!(
                    "Could not read {}: {err}",
                    location.display_text()
                ));
                self.path_entry
                    .set_text(&self.current_location.borrow().display_text());
            }
        }
    }

    fn open_dir(&self, path: PathBuf, remember: bool) {
        self.open_location(Location::Path(path), remember);
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
        let location = self.current_location.borrow().clone();
        self.open_location(location, false);
    }

    fn parent_dir(&self) -> Option<PathBuf> {
        match &*self.current_location.borrow() {
            Location::Path(path) => path.parent().map(Path::to_path_buf),
            Location::Trash | Location::Uri(_) | Location::Separator => None,
        }
    }

    fn render_entries(&self) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }

        let view_mode = *self.view_mode.borrow();
        for item in self.entries.borrow().iter() {
            let row = file_row(item, view_mode);
            attach_file_drag_source(&row, item.clone());
            let list = self.list.clone();
            let row_for_context = row.clone();
            attach_file_context_menu(&row, file_context_target(item), move || {
                list.select_row(Some(&row_for_context));
            });
            self.list.append(&row);

            let child = grid_file_child(item);
            attach_file_drag_source(&child, item.clone());
            let grid = self.grid.clone();
            let child_for_context = child.clone();
            attach_file_context_menu(&child, file_context_target(item), move || {
                grid.select_child(&child_for_context);
            });
            self.grid.append(&child);
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
        let Location::Path(current_dir) = &*self.current_location.borrow() else {
            self.set_status("New folders cannot be created in Trash.");
            self.path_entry
                .set_text(&self.current_location.borrow().display_text());
            return;
        };
        let current_dir = current_dir.clone();

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
                let path = current_dir.join(name);
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
        let Some(index) = self.selected_index() else {
            self.set_status("Select an item to move to trash.");
            return;
        };

        let Some(item) = self.entries.borrow().get(index as usize).cloned() else {
            return;
        };

        if self.current_location.borrow().is_trash() {
            self.set_status("Items in Trash are already in the trash bin.");
            return;
        }

        match item.file.trash(gio::Cancellable::NONE) {
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

    fn show_selected_path(&self) {
        let Some(index) = self.selected_index() else {
            self.path_entry
                .set_text(&self.current_location.borrow().display_text());
            return;
        };

        if let Some(item) = self.entries.borrow().get(index as usize) {
            self.path_entry.set_text(&item.display_path());
        }
    }

    fn activate_item(&self, index: i32) {
        if index < 0 {
            return;
        }

        let Some(item) = self.entries.borrow().get(index as usize).cloned() else {
            return;
        };

        if item.is_dir {
            self.open_location(item.location(), true);
        } else if let Err(err) = open_file(&item.file) {
            self.set_status(&format!("Could not open {}: {err}", item.name));
        }
    }

    fn selected_index(&self) -> Option<i32> {
        match *self.view_mode.borrow() {
            ViewMode::Grid => self
                .grid
                .selected_children()
                .first()
                .map(gtk::FlowBoxChild::index)
                .filter(|index| *index >= 0),
            ViewMode::Details | ViewMode::List => self
                .list
                .selected_row()
                .map(|row| row.index())
                .filter(|index| *index >= 0),
        }
    }

    fn set_view_mode(&self, view_mode: ViewMode) {
        if *self.view_mode.borrow() == view_mode {
            return;
        }

        *self.view_mode.borrow_mut() = view_mode;
        self.column_header
            .set_visible(matches!(view_mode, ViewMode::Details));

        match view_mode {
            ViewMode::Details | ViewMode::List => self.scroller.set_child(Some(&self.list)),
            ViewMode::Grid => self.scroller.set_child(Some(&self.grid)),
        }

        self.render_entries();
        self.path_entry
            .set_text(&self.current_location.borrow().display_text());
    }

    fn install_drop_target(&self, widget: &impl IsA<gtk::Widget>) {
        let target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );

        let this = self.clone();
        target.connect_drop(move |_, value, _, _| {
            let Ok(file_list) = value.get::<gtk::gdk::FileList>() else {
                this.set_status("Drop did not contain files.");
                return false;
            };

            this.copy_dropped_files(file_list.files())
        });

        widget.add_controller(target);
    }

    fn copy_dropped_files(&self, files: Vec<gio::File>) -> bool {
        let Location::Path(target_dir) = &*self.current_location.borrow() else {
            self.set_status("Files can only be dropped into local folders.");
            return false;
        };

        if files.is_empty() {
            self.set_status("Drop did not contain files.");
            return false;
        }

        let target_dir = target_dir.clone();
        let mut copied = 0usize;
        let mut last_error = None;

        for file in files {
            match copy_dropped_file(&file, &target_dir) {
                Ok(()) => copied += 1,
                Err(err) => last_error = Some(err),
            }
        }

        self.reload();

        match (copied, last_error) {
            (0, Some(err)) => {
                self.set_status(&format!("Could not copy dropped files: {err}"));
                false
            }
            (count, Some(err)) => {
                self.set_status(&format!(
                    "Copied {count} item{}; some items failed: {err}",
                    plural(count)
                ));
                true
            }
            (count, None) => {
                self.set_status(&format!("Copied {count} dropped item{}.", plural(count)));
                true
            }
        }
    }
}

impl FileItem {
    fn location(&self) -> Location {
        self.path
            .as_ref()
            .map(|path| Location::Path(path.clone()))
            .unwrap_or_else(|| Location::Uri(self.uri.clone()))
    }

    fn display_path(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.uri.clone())
    }
}

impl Location {
    fn display_text(&self) -> String {
        match self {
            Location::Path(path) => path.to_string_lossy().into_owned(),
            Location::Trash => "trash:///".to_string(),
            Location::Uri(uri) => uri.clone(),
            Location::Separator => "-".to_string(),
        }
    }

    fn is_trash(&self) -> bool {
        match self {
            Location::Trash => true,
            Location::Uri(uri) => uri.starts_with("trash:"),
            Location::Path(_) | Location::Separator => false,
        }
    }
}

fn read_location_items(
    location: &Location,
    show_hidden: bool,
) -> Result<Vec<FileItem>, glib::Error> {
    match location {
        Location::Path(path) => read_file_items(&gio::File::for_path(path), show_hidden),
        Location::Trash => read_file_items(&gio::File::for_uri("trash:///"), show_hidden),
        Location::Uri(uri) => read_file_items(&gio::File::for_uri(uri), show_hidden),
        Location::Separator => Ok(Vec::new()),
    }
}

fn read_file_items(file: &gio::File, show_hidden: bool) -> Result<Vec<FileItem>, glib::Error> {
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
        let child = enumerator.child(&info);
        items.push(FileItem {
            name,
            path: child.path(),
            uri: child.uri().to_string(),
            file: child,
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

fn file_row(item: &FileItem, view_mode: ViewMode) -> gtk::ListBoxRow {
    match view_mode {
        ViewMode::Details => detail_file_row(item),
        ViewMode::List | ViewMode::Grid => list_file_row(item),
    }
}

fn detail_file_row(item: &FileItem) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_hexpand(true);

    let icon = gtk::Image::from_icon_name(file_icon_name(item));
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

fn list_file_row(item: &FileItem) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-list-row");

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_hexpand(true);

    let icon = gtk::Image::from_icon_name(file_icon_name(item));
    icon.set_pixel_size(28);
    content.append(&icon);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);

    let name = gtk::Label::new(Some(&item.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("file-name");
    text.append(&name);

    let meta_text = if item.is_dir {
        "Folder".to_string()
    } else {
        format!("{} - {}", format_size(item.size), item.modified)
    };
    let meta = gtk::Label::new(Some(&meta_text));
    meta.set_xalign(0.0);
    meta.add_css_class("dim-label");
    text.append(&meta);

    content.append(&text);
    row.set_child(Some(&content));
    row
}

fn grid_file_child(item: &FileItem) -> gtk::FlowBoxChild {
    let child = gtk::FlowBoxChild::new();
    child.add_css_class("file-grid-child");

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 6);
    tile.add_css_class("file-grid-tile");
    tile.set_halign(gtk::Align::Center);

    let icon = gtk::Image::from_icon_name(file_icon_name(item));
    icon.set_pixel_size(48);
    tile.append(&icon);

    let name = gtk::Label::new(Some(&item.name));
    name.set_xalign(0.5);
    name.set_justify(gtk::Justification::Center);
    name.set_wrap(true);
    name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    name.set_lines(2);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("file-grid-name");
    tile.append(&name);

    child.set_child(Some(&tile));
    child
}

fn file_icon_name(item: &FileItem) -> &'static str {
    if item.is_dir {
        "folder-symbolic"
    } else {
        "text-x-generic-symbolic"
    }
}

fn file_context_target(item: &FileItem) -> FileContextTarget {
    if item.is_dir {
        FileContextTarget::Folder
    } else {
        FileContextTarget::File
    }
}

fn attach_file_context_menu(
    widget: &impl IsA<gtk::Widget>,
    target: FileContextTarget,
    select_item: impl Fn() + 'static,
) {
    let menu = gio::Menu::new();

    menu.append(Some("Open"), Some("win.file-open"));

    menu.append_section(None, &{
        let section = gio::Menu::new();
        section.append(Some("Cut\tCtrl+X"), Some("win.file-cut"));
        section.append(Some("Copy\tCtrl+C"), Some("win.file-copy"));
        section.append(Some("Move to..."), Some("win.file-move-to"));
        section.append(Some("Copy to..."), Some("win.file-copy-to"));
        section
    });

    menu.append_section(None, &{
        let section = gio::Menu::new();
        section.append(Some("Rename...\tF2"), Some("win.file-rename"));
        if matches!(target, FileContextTarget::Folder) {
            section.append(
                Some("Paste Into Folder"),
                Some("win.file-paste-into-folder"),
            );
        }
        section.append(Some("Compress..."), Some("win.file-compress"));
        section.append(
            Some("Move to Trash\tDelete"),
            Some("win.file-move-to-trash"),
        );
        section
    });

    menu.append_section(None, &{
        let section = gio::Menu::new();
        section.append(Some("Properties\tAlt+Return"), Some("win.file-properties"));
        section
    });

    let click = gtk::GestureClick::new();
    click.set_button(3);

    click.connect_pressed(move |gesture, _, x, y| {
        select_item();
        if let Some(parent) = gesture.widget() {
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_has_arrow(false);
            popover.set_parent(&parent);
            popover.connect_closed(|popover| popover.unparent());
            let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    widget.add_controller(click);
}

fn attach_file_drag_source(widget: &impl IsA<gtk::Widget>, item: FileItem) {
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::COPY);

    source.connect_prepare(move |_, _, _| {
        let files = [item.file.clone()];
        let file_list = gtk::gdk::FileList::from_array(&files);
        let file_provider = gtk::gdk::ContentProvider::for_value(&file_list.to_value());

        let uri_list = format!("{}\r\n", item.uri);
        let uri_provider = gtk::gdk::ContentProvider::for_bytes(
            "text/uri-list",
            &glib::Bytes::from_owned(uri_list.into_bytes()),
        );

        let display_path = item
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| item.uri.clone());
        let drag_text = format!("{}\n{}", item.name, display_path);
        let text_provider = gtk::gdk::ContentProvider::for_value(&drag_text.to_value());

        Some(gtk::gdk::ContentProvider::new_union(&[
            file_provider,
            uri_provider,
            text_provider,
        ]))
    });

    widget.add_controller(source);
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
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_hexpand(true);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(10);
        row.set_margin_end(10);

        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);

        item.downcast_ref::<gtk::ListItem>()
            .expect("ListItem")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let row = item.child().and_downcast::<gtk::Box>().expect("Box child");
        let label = row
            .first_child()
            .and_downcast::<gtk::Label>()
            .expect("Label child");
        let Some(place) = item.item().and_downcast::<gtk::StringObject>() else {
            return;
        };

        let name = place.string();
        let title = name.lines().next().unwrap_or_default();
        let path = name.lines().nth(1).unwrap_or_default();
        label.set_text(title);
        label.set_tooltip_text(Some(path));
        attach_sidebar_context_menu(&row, sidebar_kind_for_place(path));
    });
    factory
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn toggle_icon_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::new();
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn open_file(file: &gio::File) -> Result<(), glib::Error> {
    let path = file.path();

    if path
        .as_deref()
        .is_some_and(|path| path.extension().is_some_and(|ext| ext == "desktop"))
    {
        let path = path.as_deref().expect("checked desktop path");
        return launch_desktop_entry(path).map_err(|err| {
            glib::Error::new(
                gio::IOErrorEnum::Failed,
                &format!("Could not launch desktop entry: {err}"),
            )
        });
    }

    let force_x11 = path.as_deref().is_some_and(|path| {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    });
    let uri = file.uri();
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

fn copy_dropped_file(file: &gio::File, target_dir: &Path) -> io::Result<()> {
    let name = file.basename().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "dropped file has no basename")
    })?;
    let destination = available_destination(target_dir, Path::new(&name));

    if let Some(source) = file.path() {
        if source.is_dir() && is_path_inside(target_dir, &source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot copy a folder into itself",
            ));
        }
        copy_path_recursively(&source, &destination)
    } else {
        let destination_file = gio::File::for_path(&destination);
        file.copy(
            &destination_file,
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }
}

fn is_path_inside(path: &Path, parent: &Path) -> bool {
    match (path.canonicalize(), parent.canonicalize()) {
        (Ok(path), Ok(parent)) => path.starts_with(parent),
        _ => false,
    }
}

fn copy_path_recursively(source: &Path, destination: &Path) -> io::Result<()> {
    if source.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_path_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(source, destination).map(|_| ())
    }
}

fn available_destination(target_dir: &Path, basename: &Path) -> PathBuf {
    let mut destination = target_dir.join(basename);
    if !destination.exists() {
        return destination;
    }

    let stem = basename
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled");
    let extension = basename
        .extension()
        .and_then(|extension| extension.to_str());

    for index in 1.. {
        let file_name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem} copy {index}.{extension}"),
            _ => format!("{stem} copy {index}"),
        };
        destination = target_dir.join(file_name);
        if !destination.exists() {
            return destination;
        }
    }

    unreachable!("unbounded destination search should always return")
}

fn location_from_entry(text: &str) -> Location {
    let text = text.trim();
    if text.eq_ignore_ascii_case("trash:///") || text.eq_ignore_ascii_case("trash://") {
        Location::Trash
    } else if text.starts_with("trash:") {
        Location::Uri(text.to_string())
    } else {
        Location::Path(PathBuf::from(text))
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
        .open("/tmp/focaldesk-files.log")
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

fn normalize_location(location: Location) -> Location {
    match location {
        Location::Path(path) => Location::Path(normalize_path(&path)),
        Location::Uri(uri) if uri == "trash:///" || uri == "trash://" => Location::Trash,
        other => other,
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn ensure_standard_user_dirs() {
    for name in ["Desktop", "Downloads", "Music", "Pictures", "Videos"] {
        let path = home_dir().join(name);
        if let Err(err) = std::fs::create_dir_all(&path) {
            log_launch(&format!("could not create {}: {err}", path.display()));
        }
    }
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

fn sidebar_kind_for_place(path: &str) -> SidebarKind {
    match path {
        "trash:///" => SidebarKind::Trash,
        "-" => SidebarKind::Separator,
        _ => SidebarKind::Folder,
    }
}

fn attach_sidebar_context_menu(widget: &impl IsA<gtk::Widget>, kind: SidebarKind) {
    if matches!(kind, SidebarKind::Separator) {
        return;
    }

    let menu = gio::Menu::new();

    menu.append(Some("Open"), Some("win.sidebar-open"));

    if matches!(kind, SidebarKind::Folder) {
        menu.append(Some("Open in New Tab"), Some("win.sidebar-open-tab"));
        menu.append(Some("Open in New Window"), Some("win.sidebar-open-window"));
    }

    menu.append_section(None, &{
        let section = gio::Menu::new();

        if matches!(kind, SidebarKind::Trash) {
            section.append(Some("Empty Trash..."), Some("win.empty-trash"));
        }

        section
    });

    menu.append_section(None, &{
        let section = gio::Menu::new();
        section.append(Some("Properties"), Some("win.sidebar-properties"));
        section
    });

    let click = gtk::GestureClick::new();
    click.set_button(3);

    click.connect_pressed(move |gesture, _, x, y| {
        if let Some(parent) = gesture.widget() {
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_has_arrow(false);
            popover.set_parent(&parent);
            popover.connect_closed(|popover| popover.unparent());
            let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    widget.add_controller(click);
}

fn install_sidebar_actions(window: &gtk::ApplicationWindow) {
    let open = gio::SimpleAction::new("sidebar-open", None);
    open.connect_activate(|_, _| {
        flog_info!("Open sidebar location");
    });
    window.add_action(&open);

    let open_tab = gio::SimpleAction::new("sidebar-open-tab", None);
    open_tab.connect_activate(|_, _| {
        flog_info!("Open in new tab");
    });
    window.add_action(&open_tab);

    let open_window = gio::SimpleAction::new("sidebar-open-window", None);
    open_window.connect_activate(|_, _| {
        flog_info!("Open in new window");
    });
    window.add_action(&open_window);

    let empty_trash = gio::SimpleAction::new("empty-trash", None);
    empty_trash.connect_activate(|_, _| {
        flog_info!("Empty Trash...");
    });
    window.add_action(&empty_trash);

    let properties = gio::SimpleAction::new("sidebar-properties", None);
    properties.connect_activate(|_, _| {
        flog_info!("Show properties");
    });
    window.add_action(&properties);
}
