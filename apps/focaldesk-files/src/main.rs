use adw::prelude::*;
use focaldesk_logging::{init_default_logging, session_id};
use glib::ControlFlow;
use gtk::gio;
use gtk::gio::prelude::AppInfoExt;
use gtk::glib;
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use tracing::info;

#[derive(Debug, Clone)]
struct FileItem {
    name: String,
    file: gio::File,
    path: Option<PathBuf>,
    uri: String,
    is_dir: bool,
    size: u64,
    modified: String,
    content_type: String,
    icon: gio::Icon,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Name,
    Size,
    Type,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardOp {
    Copy,
    Cut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileJobOutcome {
    Finished,
    Cancelled,
}

#[derive(Debug, Clone)]
struct FileClipboard {
    files: Vec<gio::File>,
    op: ClipboardOp,
}

const FILE_TRANSFER_MIME_TYPES: &[&str] = &["x-special/gnome-copied-files", "text/uri-list"];
const DIRECTORY_RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
const DIRECTORY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone)]
struct FileManager {
    window: adw::ApplicationWindow,
    root: adw::ToolbarView,
    tab_name: String,
    path_entry: gtk::Entry,
    list: gtk::ListBox,
    grid: gtk::FlowBox,
    scroller: gtk::ScrolledWindow,
    column_header: gtk::Grid,
    status: gtk::Label,
    hidden_toggle: gtk::ToggleButton,
    places: gtk::StringList,
    view_mode: Rc<RefCell<ViewMode>>,
    sort_mode: Rc<RefCell<SortMode>>,
    sort_descending: Rc<RefCell<bool>>,
    search_query: Rc<RefCell<String>>,
    current_location: Rc<RefCell<Location>>,
    entries: Rc<RefCell<Vec<FileItem>>>,
    visible_entries: Rc<RefCell<Vec<FileItem>>>,
    file_clipboard: Rc<RefCell<Option<FileClipboard>>>,
    back_stack: Rc<RefCell<Vec<Location>>>,
    forward_stack: Rc<RefCell<Vec<Location>>>,
    context_menu_index: Rc<RefCell<Option<i32>>>,
    tab_page: Rc<RefCell<Option<gtk::StackPage>>>,
    directory_monitor: Rc<RefCell<Option<gio::FileMonitor>>>,
    pending_directory_reload: Rc<RefCell<Option<glib::SourceId>>>,
    directory_poll: Rc<RefCell<Option<glib::SourceId>>>,
    directory_revision: Rc<RefCell<Option<(std::time::SystemTime, usize)>>>,
}

#[derive(Clone)]
struct TabManager {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    tabs: Rc<RefCell<Vec<FileManager>>>,
    tab_counter: Rc<RefCell<u32>>,
}

impl TabManager {
    fn new(window: &adw::ApplicationWindow, stack: &gtk::Stack) -> Self {
        Self {
            window: window.clone(),
            stack: stack.clone(),
            tabs: Rc::new(RefCell::new(Vec::new())),
            tab_counter: Rc::new(RefCell::new(1)),
        }
    }

    fn next_tab_name(&self) -> String {
        let mut counter = self.tab_counter.borrow_mut();
        let name = format!("tab-{}", *counter);
        *counter += 1;
        name
    }

    fn active_tab(&self) -> Option<FileManager> {
        let active_name = self.stack.visible_child_name()?;
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.tab_name == active_name.as_str())
            .cloned()
    }

    fn with_active_tab(&self, mut callback: impl FnMut(&FileManager)) {
        if let Some(tab) = self.active_tab() {
            callback(&tab);
        }
    }

    fn install_actions(&self) {
        let open = gio::SimpleAction::new("sidebar-open", Some(&String::static_variant_type()));
        let this = self.clone();
        open.connect_activate(move |_, parameter| {
            let Some(location) = sidebar_location_from_parameter(parameter) else {
                return;
            };
            this.with_active_tab(|tab| tab.open_location(location.clone(), true));
        });
        self.window.add_action(&open);

        let open_tab =
            gio::SimpleAction::new("sidebar-open-tab", Some(&String::static_variant_type()));
        let this = self.clone();
        open_tab.connect_activate(move |_, parameter| {
            let Some(location) = sidebar_location_from_parameter(parameter) else {
                return;
            };
            this.add_location_tab(location);
        });
        self.window.add_action(&open_tab);

        let open_window =
            gio::SimpleAction::new("sidebar-open-window", Some(&String::static_variant_type()));
        open_window.connect_activate(|_, parameter| {
            let Some(location) = sidebar_location_from_parameter(parameter) else {
                return;
            };

            if let Err(err) = open_sidebar_location_in_new_window(&location) {
                info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    action = "sidebar-open-window",
                    error = %err,
                    "open new window failed"
                );
            }
        });
        self.window.add_action(&open_window);

        let empty_trash = gio::SimpleAction::new("empty-trash", None);
        empty_trash.connect_activate(|_, _| {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                action = "empty-trash",
                "empty trash"
            );
        });
        self.window.add_action(&empty_trash);

        let properties =
            gio::SimpleAction::new("sidebar-properties", Some(&String::static_variant_type()));
        properties.connect_activate(|_, parameter| {
            let Some(location) = sidebar_location_from_parameter(parameter) else {
                return;
            };
            info!(
                target: "focaldesk",
                session_id = session_id(),
                action = "sidebar-properties",
                location = %location.display_text(),
                "sidebar properties"
            );
        });
        self.window.add_action(&properties);

        let tab_new = gio::SimpleAction::new("tab-new", None);
        let this = self.clone();
        tab_new.connect_activate(move |_, _| {
            this.add_location_tab(Location::Path(home_dir()));
        });
        self.window.add_action(&tab_new);

        let tab_close = gio::SimpleAction::new("tab-close", None);
        let this = self.clone();
        tab_close.connect_activate(move |_, _| {
            this.close_active_tab();
        });
        self.window.add_action(&tab_close);

        let file_open = gio::SimpleAction::new("file-open", None);
        let this = self.clone();
        file_open.connect_activate(move |_, _| {
            this.with_active_tab(|tab| {
                let tab = tab.clone();
                glib::idle_add_local_once(move || {
                    if let Some(index) = tab.selected_index_for_action() {
                        tab.activate_item(index);
                    }
                });
            });
        });
        self.window.add_action(&file_open);

        let file_open_with = gio::SimpleAction::new("file-open-with", None);
        let this = self.clone();
        file_open_with.connect_activate(move |_, _| {
            info!(target: "focaldesk", "file-open-with activated");
            this.with_active_tab(|tab| {
                let tab = tab.clone();
                glib::idle_add_local_once(move || tab.show_open_with_dialog());
            });
        });
        self.window.add_action(&file_open_with);

        let file_cut = gio::SimpleAction::new("file-cut", None);
        let this = self.clone();
        file_cut.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.copy_selected_to_clipboard(ClipboardOp::Cut));
        });
        self.window.add_action(&file_cut);

        let file_copy = gio::SimpleAction::new("file-copy", None);
        let this = self.clone();
        file_copy.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.copy_selected_to_clipboard(ClipboardOp::Copy));
        });
        self.window.add_action(&file_copy);

        let file_paste = gio::SimpleAction::new("file-paste", None);
        let this = self.clone();
        file_paste.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.paste_files_to_current_folder());
        });
        self.window.add_action(&file_paste);

        let file_move_to = gio::SimpleAction::new("file-move-to", None);
        file_move_to.connect_activate(|_, _| {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                action = "file-move-to",
                "move file item to"
            );
        });
        self.window.add_action(&file_move_to);

        let file_copy_to = gio::SimpleAction::new("file-copy-to", None);
        file_copy_to.connect_activate(|_, _| {
            info!(
                target: "focaldesk",
                session_id = session_id(),
                action = "file-copy-to",
                "copy file item to"
            );
        });
        self.window.add_action(&file_copy_to);

        let file_open_tab = gio::SimpleAction::new("file-open-tab", None);
        let this = self.clone();
        file_open_tab.connect_activate(move |_, _| {
            this.with_active_tab(|tab| {
                if let Some(index) = tab.selected_index_for_action() {
                    if let Some(item) = tab.visible_entries.borrow().get(index as usize).cloned() {
                        if item.is_dir {
                            this.add_location_tab(item.location());
                        }
                    }
                }
            });
        });
        self.window.add_action(&file_open_tab);

        let file_rename = gio::SimpleAction::new("file-rename", None);
        let this = self.clone();
        file_rename.connect_activate(move |_, _| {
            this.with_active_tab(|tab| {
                let tab = tab.clone();
                glib::idle_add_local_once(move || tab.show_rename_dialog());
            });
        });
        self.window.add_action(&file_rename);

        let file_select_all = gio::SimpleAction::new("file-select-all", None);
        let this = self.clone();
        file_select_all.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.select_all_visible_items());
        });
        self.window.add_action(&file_select_all);

        let file_paste_into_folder = gio::SimpleAction::new("file-paste-into-folder", None);
        let this = self.clone();
        file_paste_into_folder.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.paste_files_into_selected_folder());
        });
        self.window.add_action(&file_paste_into_folder);

        let file_compress = gio::SimpleAction::new("file-compress", None);
        let this = self.clone();
        file_compress.connect_activate(move |_, _| {
            this.with_active_tab(|tab| {
                let tab = tab.clone();
                glib::idle_add_local_once(move || tab.show_compress_dialog());
            });
        });
        self.window.add_action(&file_compress);

        let file_move_to_trash = gio::SimpleAction::new("file-move-to-trash", None);
        let this = self.clone();
        file_move_to_trash.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.trash_selected());
        });
        self.window.add_action(&file_move_to_trash);

        let file_properties = gio::SimpleAction::new("file-properties", None);
        let this = self.clone();
        file_properties.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.show_properties_dialog());
        });
        self.window.add_action(&file_properties);
    }

    fn close_active_tab(&self) {
        let Some(active_name) = self.stack.visible_child_name() else {
            return;
        };

        let mut tabs = self.tabs.borrow_mut();
        if tabs.len() <= 1 {
            self.window.close();
            return;
        }

        let Some(index) = tabs
            .iter()
            .position(|tab| tab.tab_name == active_name.as_str())
        else {
            return;
        };

        let tab = tabs.remove(index);
        self.stack.remove(&tab.root);

        let next_index = if index > 0 { index - 1 } else { 0 };
        if let Some(next_tab) = tabs.get(next_index) {
            self.stack.set_visible_child_name(&next_tab.tab_name);
        }
    }

    fn add_location_tab(&self, location: Location) {
        let tab_name = self.next_tab_name();
        let tab_name_for_stack = tab_name.clone();
        let tab = create_file_manager_page(&self.window, tab_name);
        let page = self.stack.add_titled(
            &tab.root,
            Some(&tab.tab_name),
            &tab_title_for_location(&location),
        );
        tab.set_tab_page(page);
        match location {
            Location::Path(path) => tab.open_initial_path(path),
            other => tab.open_location(other, false),
        }
        self.tabs.borrow_mut().push(tab);
        self.stack.set_visible_child_name(&tab_name_for_stack);
    }
}

fn main() {
    init_default_logging();
    let app = adw::Application::new(
        Some("com.focaldesk.Files"),
        gio::ApplicationFlags::HANDLES_OPEN,
    );
    app.connect_activate(|app| build_ui(app, None));
    app.connect_open(|app, files, _| {
        let initial_path = files.first().and_then(|file| file.path());
        build_ui(app, initial_path);
    });
    app.set_accels_for_action("win.file-cut", &["<Control>x"]);
    app.set_accels_for_action("win.file-copy", &["<Control>c"]);
    app.set_accels_for_action("win.file-paste", &["<Control>v"]);
    app.set_accels_for_action("win.file-move-to-trash", &["Delete"]);
    app.set_accels_for_action("win.file-rename", &["F2"]);
    app.set_accels_for_action("win.file-properties", &["<Alt>Return"]);
    app.set_accels_for_action("win.file-select-all", &["<Control>a"]);
    app.set_accels_for_action("win.tab-new", &["<Control>t"]);
    app.set_accels_for_action("win.tab-close", &["<Control>w"]);
    app.run();
}

fn build_ui(app: &adw::Application, initial_path: Option<PathBuf>) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("FocalDesk Files"));
    window.set_default_size(1040, 680);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_hexpand(true);

    let new_tab_button = icon_button("tab-new-symbolic", "New Tab");
    let tab_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    tab_bar.set_margin_top(8);
    tab_bar.set_margin_bottom(8);
    tab_bar.set_margin_start(12);
    tab_bar.set_margin_end(12);
    tab_bar.append(&switcher);
    tab_bar.append(&new_tab_button);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&tab_bar);
    outer.append(&stack);
    window.set_content(Some(&outer));

    let tabs = TabManager::new(&window, &stack);
    tabs.install_actions();

    let tabs_for_button = tabs.clone();
    new_tab_button.connect_clicked(move |_| {
        gtk::prelude::ActionGroupExt::activate_action(&tabs_for_button.window, "tab-new", None);
    });

    ensure_standard_user_dirs();
    tabs.add_location_tab(Location::Path(initial_path.unwrap_or_else(home_dir)));
    window.present();
}

fn create_file_manager_page(window: &adw::ApplicationWindow, tab_name: String) -> FileManager {
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
    close_button.connect_clicked(move |_| {
        gtk::prelude::ActionGroupExt::activate_action(&window_for_close, "tab-close", None);
    });
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
    list.set_selection_mode(gtk::SelectionMode::Multiple);
    list.set_activate_on_single_click(false);
    list.add_css_class("boxed-list");
    list.set_vexpand(true);

    let grid = gtk::FlowBox::new();
    grid.set_selection_mode(gtk::SelectionMode::Multiple);
    grid.set_activate_on_single_click(false);
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

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_hexpand(true);
    search_entry.set_placeholder_text(Some("Search current folder"));

    let sort_dropdown = gtk::DropDown::from_strings(&["Name", "Size", "Type", "Modified"]);
    sort_dropdown.set_selected(0);

    let sort_descending = gtk::ToggleButton::new();
    sort_descending.set_icon_name("view-sort-descending-symbolic");
    sort_descending.set_tooltip_text(Some("Toggle sort order"));

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.set_margin_top(8);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.set_margin_top(8);
    controls.append(&search_entry);
    controls.append(&sort_dropdown);
    controls.append(&sort_descending);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&controls);
    let column_header = column_header();
    content.append(&column_header);
    content.append(&scroller);
    content.append(&status);
    split.set_content(Some(&adw::NavigationPage::new(&content, "Files")));

    toolbar.set_content(Some(&split));

    let manager = FileManager {
        window: window.clone(),
        root: toolbar,
        tab_name,
        path_entry,
        list,
        grid,
        scroller,
        column_header,
        status,
        hidden_toggle,
        places,
        view_mode: Rc::new(RefCell::new(ViewMode::Details)),
        sort_mode: Rc::new(RefCell::new(SortMode::Name)),
        sort_descending: Rc::new(RefCell::new(false)),
        search_query: Rc::new(RefCell::new(String::new())),
        current_location: Rc::new(RefCell::new(Location::Path(home_dir()))),
        entries: Rc::new(RefCell::new(Vec::new())),
        visible_entries: Rc::new(RefCell::new(Vec::new())),
        file_clipboard: Rc::new(RefCell::new(None)),
        back_stack: Rc::new(RefCell::new(Vec::new())),
        forward_stack: Rc::new(RefCell::new(Vec::new())),
        context_menu_index: Rc::new(RefCell::new(None)),
        tab_page: Rc::new(RefCell::new(None)),
        directory_monitor: Rc::new(RefCell::new(None)),
        pending_directory_reload: Rc::new(RefCell::new(None)),
        directory_poll: Rc::new(RefCell::new(None)),
        directory_revision: Rc::new(RefCell::new(None)),
    };

    ensure_standard_user_dirs();
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
        details_view_button,
        list_view_button,
        grid_view_button,
        places_view,
    );
    manager.connect_search_and_sort_actions(search_entry, sort_dropdown, sort_descending);
    manager
}

impl FileManager {
    fn connect_search_and_sort_actions(
        &self,
        search_entry: gtk::SearchEntry,
        sort_dropdown: gtk::DropDown,
        sort_descending: gtk::ToggleButton,
    ) {
        let this = self.clone();
        search_entry.connect_search_changed(move |entry| {
            this.set_search_query(entry.text().to_string());
        });

        let this = self.clone();
        sort_dropdown.connect_selected_notify(move |dropdown| {
            this.set_sort_mode(sort_mode_from_dropdown(dropdown.selected()));
        });

        let this = self.clone();
        sort_descending.connect_toggled(move |button| {
            this.set_sort_descending(button.is_active());
        });
    }

    #[allow(clippy::too_many_arguments)]
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
        self.watch_file_clipboard_owner();

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
                let location_changed = *self.current_location.borrow() != location;
                if remember && *self.current_location.borrow() != location {
                    self.back_stack
                        .borrow_mut()
                        .push(self.current_location.borrow().clone());
                    self.forward_stack.borrow_mut().clear();
                }

                *self.current_location.borrow_mut() = location.clone();
                *self.directory_revision.borrow_mut() = local_directory_revision(&location);
                *self.entries.borrow_mut() = items;
                self.path_entry.set_text(&location.display_text());
                self.render_entries();
                self.update_tab_title();
                log_launch(&format!("location opened: {}", location.display_text()));
                if location_changed || self.directory_monitor.borrow().is_none() {
                    self.monitor_location(&location);
                }
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

    fn set_tab_page(&self, page: gtk::StackPage) {
        self.tab_page.borrow_mut().replace(page);
        self.update_tab_title();
    }

    fn update_tab_title(&self) {
        let tab_page = self.tab_page.borrow();
        let Some(page) = tab_page.as_ref() else {
            return;
        };

        page.set_title(&tab_title_for_location(&self.current_location.borrow()));
    }

    fn reload(&self) {
        log_launch(&format!(
            "reloading: {}",
            self.current_location.borrow().display_text()
        ));
        let selected_uris = self
            .selected_file_items()
            .into_iter()
            .map(|item| item.uri)
            .collect::<Vec<_>>();
        let scroll_position = self.scroller.vadjustment().value();
        let location = self.current_location.borrow().clone();
        self.open_location(location, false);
        self.restore_selection(&selected_uris);

        let adjustment = self.scroller.vadjustment();
        glib::idle_add_local_once(move || {
            let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
            adjustment.set_value(scroll_position.clamp(adjustment.lower(), maximum));
        });
    }

    fn restore_selection(&self, selected_uris: &[String]) {
        for (index, item) in self.visible_entries.borrow().iter().enumerate() {
            if !selected_uris.contains(&item.uri) {
                continue;
            }

            match *self.view_mode.borrow() {
                ViewMode::Grid => {
                    if let Some(child) = self.grid.child_at_index(index as i32) {
                        self.grid.select_child(&child);
                    }
                }
                ViewMode::Details | ViewMode::List => {
                    if let Some(row) = self.list.row_at_index(index as i32) {
                        self.list.select_row(Some(&row));
                    }
                }
            }
        }
    }

    fn monitor_location(&self, location: &Location) {
        log_launch(&format!(
            "installing directory watch: {}",
            location.display_text()
        ));
        if let Some(source_id) = self.pending_directory_reload.borrow_mut().take() {
            source_id.remove();
        }
        if let Some(source_id) = self.directory_poll.borrow_mut().take() {
            source_id.remove();
        }
        if let Some(monitor) = self.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }

        if matches!(location, Location::Path(_)) {
            let this = self.clone();
            let source_id = glib::timeout_add_local(DIRECTORY_POLL_INTERVAL, move || {
                if this.root.parent().is_none() {
                    log_launch("directory poll stopped because tab is detached");
                    return ControlFlow::Break;
                }

                let revision = local_directory_revision(&this.current_location.borrow());
                let changed = revision != *this.directory_revision.borrow();
                if changed {
                    log_launch(&format!(
                        "directory poll detected change: {}",
                        this.current_location.borrow().display_text()
                    ));
                    *this.directory_revision.borrow_mut() = revision;
                    this.queue_directory_reload();
                }
                ControlFlow::Continue
            });
            self.directory_poll.borrow_mut().replace(source_id);
        }

        let file = match location {
            Location::Path(path) => gio::File::for_path(path),
            Location::Trash => gio::File::for_uri("trash:///"),
            Location::Uri(uri) => gio::File::for_uri(uri),
            Location::Separator => return,
        };
        let monitor = match file
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            Ok(monitor) => monitor,
            Err(err) => {
                info!(
                    target: "focaldesk",
                    location = %location.display_text(),
                    error = %err,
                    "directory monitoring unavailable"
                );
                return;
            }
        };

        monitor.set_rate_limit(DIRECTORY_RELOAD_DEBOUNCE.as_millis() as i32);
        let this = self.clone();
        monitor.connect_changed(move |_, file, _, event| {
            log_launch(&format!(
                "directory monitor event {event:?}: {}",
                file.uri()
            ));
            this.queue_directory_reload();
        });
        self.directory_monitor.borrow_mut().replace(monitor);
    }

    fn queue_directory_reload(&self) {
        log_launch("directory reload queued");
        if let Some(source_id) = self.pending_directory_reload.borrow_mut().take() {
            source_id.remove();
        }

        let this = self.clone();
        let pending_reload = self.pending_directory_reload.clone();
        let source_id = glib::timeout_add_local_once(DIRECTORY_RELOAD_DEBOUNCE, move || {
            pending_reload.borrow_mut().take();
            this.reload();
        });
        self.pending_directory_reload
            .borrow_mut()
            .replace(source_id);
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
        let search_query = self.search_query.borrow().clone();
        let sort_mode = *self.sort_mode.borrow();
        let sort_descending = *self.sort_descending.borrow();
        let mut visible_entries = self.entries.borrow().clone();
        visible_entries.retain(|item| file_matches_search(item, &search_query));
        sort_file_items(&mut visible_entries, sort_mode, sort_descending);
        *self.visible_entries.borrow_mut() = visible_entries.clone();

        for item in visible_entries.iter() {
            let row = file_row(item, view_mode);
            attach_file_drag_source(&row, item.clone());
            let list = self.list.clone();
            let row_for_context = row.clone();
            let context_index = self.context_menu_index.clone();
            let this = self.clone();
            let item_for_rename = item.clone();
            attach_file_context_menu(
                &row,
                file_context_target(item),
                move || {
                    list.unselect_all();
                    list.select_row(Some(&row_for_context));
                    *context_index.borrow_mut() = Some(row_for_context.index());
                },
                move || {
                    let this = this.clone();
                    let item = item_for_rename.clone();
                    glib::idle_add_local_once(move || this.show_rename_dialog_for_item(item));
                },
            );
            self.list.append(&row);

            let child = grid_file_child(item);
            attach_file_drag_source(&child, item.clone());
            let grid = self.grid.clone();
            let child_for_context = child.clone();
            let context_index = self.context_menu_index.clone();
            let this = self.clone();
            let item_for_rename = item.clone();
            attach_file_context_menu(
                &child,
                file_context_target(item),
                move || {
                    grid.unselect_all();
                    grid.select_child(&child_for_context);
                    *context_index.borrow_mut() = Some(child_for_context.index());
                },
                move || {
                    let this = this.clone();
                    let item = item_for_rename.clone();
                    glib::idle_add_local_once(move || this.show_rename_dialog_for_item(item));
                },
            );
            self.grid.append(&child);
        }

        let entries = self.entries.borrow();
        let total_folders = entries.iter().filter(|item| item.is_dir).count();
        let total_files = entries.len().saturating_sub(total_folders);
        if visible_entries.len() == entries.len() {
            self.set_status(&format!(
                "{} folder{}, {} file{}",
                total_folders,
                plural(total_folders),
                total_files,
                plural(total_files)
            ));
        } else {
            self.set_status(&format!(
                "Showing {} of {} item{}",
                visible_entries.len(),
                entries.len(),
                plural(entries.len())
            ));
        }
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

    fn show_rename_dialog(&self) {
        let context_index = self
            .context_menu_index
            .borrow_mut()
            .take()
            .filter(|index| *index >= 0);
        let index = if let Some(index) = context_index {
            index
        } else {
            if self.selected_indices().len() > 1 {
                self.set_status("Select only one item to rename.");
                return;
            }

            let Some(index) = self.selected_index() else {
                self.set_status("Select an item to rename.");
                return;
            };
            index
        };

        let Some(item) = self.visible_entries.borrow().get(index as usize).cloned() else {
            return;
        };

        self.show_rename_dialog_for_item(item);
    }

    fn show_rename_dialog_for_item(&self, item: FileItem) {
        let Some(source_path) = item.path.clone() else {
            self.set_status("Only local files and folders can be renamed.");
            return;
        };

        let Some(parent_dir) = source_path.parent().map(Path::to_path_buf) else {
            self.set_status("Cannot rename this item.");
            return;
        };

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Rename")
            .default_width(360)
            .resizable(false)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let label = gtk::Label::new(Some("New name"));
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_text(&item.name);
        entry.select_region(0, item.name.chars().count() as i32);

        let cancel = gtk::Button::with_label("Cancel");
        let rename = gtk::Button::with_label("OK");
        rename.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&rename);

        content.append(&label);
        content.append(&entry);
        content.append(&actions);
        dialog.set_child(Some(&content));
        dialog.set_default_widget(Some(&rename));

        let rename_for_enter = rename.clone();
        entry.connect_activate(move |_| rename_for_enter.emit_clicked());

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());

        let key_controller = gtk::EventControllerKey::new();
        let dialog_for_escape = dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                dialog_for_escape.close();
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);

        let this = self.clone();
        let dialog_for_rename = dialog.clone();
        rename.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if name.is_empty() || name.contains('/') {
                this.set_status("Names cannot be empty or contain '/'.");
                return;
            }

            let destination = parent_dir.join(&name);
            if destination == source_path {
                dialog_for_rename.close();
                return;
            }

            match std::fs::rename(&source_path, &destination) {
                Ok(()) => {
                    this.reload();
                    this.set_status(&format!("Renamed {} to {}.", item.name, name));
                    dialog_for_rename.close();
                }
                Err(err) => {
                    this.set_status(&format!("Could not rename {}: {err}", item.name));
                }
            }
        });

        dialog.present();
    }

    fn show_compress_dialog(&self) {
        let items = self.selected_file_items();
        if items.is_empty() {
            self.set_status("Select one or more items to compress.");
            return;
        }

        let current_dir = match &*self.current_location.borrow() {
            Location::Path(path) => path.clone(),
            Location::Trash | Location::Uri(_) | Location::Separator => {
                self.set_status("Only local files and folders can be compressed.");
                return;
            }
        };

        let mut sources = Vec::with_capacity(items.len());
        for item in &items {
            let Some(path) = item.path.clone() else {
                self.set_status("Only local files and folders can be compressed.");
                return;
            };
            sources.push(path);
        }

        let default_name = if items.len() == 1 {
            format!("{}.tar.gz", items[0].name)
        } else {
            "Archive.tar.gz".to_string()
        };

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Compress")
            .default_width(420)
            .resizable(false)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let label = gtk::Label::new(Some("Archive name"));
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_text(&default_name);
        entry.select_region(0, default_name.chars().count() as i32);

        let format = gtk::Label::new(Some("Format: gzip-compressed tar archive (.tar.gz)"));
        format.set_xalign(0.0);
        format.add_css_class("dim-label");

        let cancel = gtk::Button::with_label("Cancel");
        let compress = gtk::Button::with_label("Compress");
        compress.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&compress);

        content.append(&label);
        content.append(&entry);
        content.append(&format);
        content.append(&actions);
        dialog.set_child(Some(&content));
        dialog.set_default_widget(Some(&compress));

        let compress_for_enter = compress.clone();
        entry.connect_activate(move |_| compress_for_enter.emit_clicked());

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());

        let this = self.clone();
        let dialog_for_compress = dialog.clone();
        compress.connect_clicked(move |_| {
            let mut name = entry.text().trim().to_string();
            if name.is_empty() || name.contains('/') {
                this.set_status("Archive names cannot be empty or contain '/'.");
                return;
            }
            if !name.ends_with(".tar.gz") {
                name.push_str(".tar.gz");
            }

            let destination = current_dir.join(&name);
            if destination.exists() {
                this.set_status(&format!("An archive named {name} already exists."));
                return;
            }

            dialog_for_compress.close();
            this.set_status(&format!(
                "Compressing {} item{} into {name}...",
                sources.len(),
                plural(sources.len())
            ));

            let (tx, rx) = mpsc::channel();
            let sources = sources.clone();
            thread::spawn(move || {
                let result = create_tar_gz_archive(&sources, &destination);
                let _ = tx.send(result);
            });

            let this = this.clone();
            let name = name.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                match rx.try_recv() {
                    Ok(Ok(())) => {
                        this.reload();
                        this.set_status(&format!("Created {name}."));
                        ControlFlow::Break
                    }
                    Ok(Err(err)) => {
                        this.set_status(&format!("Could not create {name}: {err}"));
                        ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        this.set_status("Compression task ended unexpectedly.");
                        ControlFlow::Break
                    }
                }
            });
        });

        dialog.present();
    }

    fn trash_selected(&self) {
        let selected = self.selected_file_items();
        if selected.is_empty() {
            self.set_status("Select an item to move to trash.");
            return;
        };

        if self.current_location.borrow().is_trash() {
            self.set_status("Items in Trash are already in the trash bin.");
            return;
        }

        let items = selected;
        self.run_progress_job(
            "Moving to Trash",
            items.len(),
            move |index| match items.get(index) {
                Some(item) => item
                    .file
                    .trash(gio::Cancellable::NONE)
                    .map_err(|err| format!("Could not move {} to trash: {err}", item.name)),
                None => Ok(()),
            },
            {
                let this = self.clone();
                move |outcome, completed, error| match outcome {
                    FileJobOutcome::Finished => {
                        this.reload();
                        match (completed, error) {
                            (0, Some(err)) => this.set_status(&err),
                            (count, Some(err)) => this.set_status(&format!(
                                "Moved {count} item{} to trash; some items failed: {err}",
                                plural(count)
                            )),
                            (count, None) => this.set_status(&format!(
                                "Moved {count} item{} to trash.",
                                plural(count)
                            )),
                        }
                    }
                    FileJobOutcome::Cancelled => this.set_status(&format!(
                        "Cancelled after moving {completed} item{} to trash.",
                        plural(completed)
                    )),
                }
            },
        );
    }

    fn copy_selected_to_clipboard(&self, op: ClipboardOp) {
        let selected = self.selected_file_items();
        if selected.is_empty() {
            self.set_status("Select an item to copy.");
            return;
        };

        let files = selected
            .iter()
            .map(|item| item.file.clone())
            .collect::<Vec<_>>();
        let provider = file_clipboard_provider(&files, op);

        let Some(display) = gtk::gdk::Display::default() else {
            self.set_status("No display clipboard is available.");
            return;
        };

        match display.clipboard().set_content(Some(&provider)) {
            Ok(()) => {
                *self.file_clipboard.borrow_mut() = Some(FileClipboard { files, op });
                let action = match op {
                    ClipboardOp::Copy => "Copied",
                    ClipboardOp::Cut => "Cut",
                };
                self.set_status(&format!(
                    "{action} {} item{} to clipboard.",
                    selected.len(),
                    plural(selected.len())
                ));
            }
            Err(err) => self.set_status(&format!("Could not copy selection: {err}")),
        }
    }

    fn paste_files_to_current_folder(&self) {
        let Location::Path(target_dir) = &*self.current_location.borrow() else {
            self.set_status("Files can only be pasted into local folders.");
            return;
        };

        self.paste_files_into_folder(target_dir.clone());
    }

    fn paste_files_into_selected_folder(&self) {
        let Some(index) = self.selected_index_for_action() else {
            self.set_status("Select a folder to paste into.");
            return;
        };

        let Some(item) = self.entries.borrow().get(index as usize).cloned() else {
            return;
        };

        let Some(target_dir) = item.path.filter(|_| item.is_dir) else {
            self.set_status("Select a local folder to paste into.");
            return;
        };

        self.paste_files_into_folder(target_dir);
    }

    fn paste_files_into_folder(&self, target_dir: PathBuf) {
        let Some(display) = gtk::gdk::Display::default() else {
            self.set_status("No display clipboard is available.");
            return;
        };

        if display.clipboard().is_local() {
            if let Some(clipboard) = self.file_clipboard.borrow().clone() {
                self.copy_or_move_clipboard_files(clipboard.files, clipboard.op, &target_dir);
                return;
            }
        } else {
            self.file_clipboard.borrow_mut().take();
        }

        let this = self.clone();
        let clipboard = display.clipboard();
        let clipboard_for_fallback = clipboard.clone();
        clipboard.read_value_async(
            gtk::gdk::FileList::static_type(),
            glib::Priority::default(),
            gio::Cancellable::NONE,
            move |result| {
                if let Ok(file_list) = result.and_then(|value| {
                    value
                        .get::<gtk::gdk::FileList>()
                        .map_err(|_| glib::Error::new(gio::IOErrorEnum::InvalidData, "not files"))
                }) {
                    this.copy_or_move_clipboard_files(
                        file_list.files(),
                        ClipboardOp::Copy,
                        &target_dir,
                    );
                    return;
                }

                this.paste_mime_files_into_folder(clipboard_for_fallback, target_dir);
            },
        );
    }

    fn paste_mime_files_into_folder(&self, clipboard: gtk::gdk::Clipboard, target_dir: PathBuf) {
        let this = self.clone();
        clipboard.read_async(
            FILE_TRANSFER_MIME_TYPES,
            glib::Priority::default(),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok((stream, mime_type)) => {
                    let this = this.clone();
                    read_transfer_text(stream, move |text| {
                        match file_clipboard_from_text(mime_type.as_str(), &text) {
                            Some(clipboard) => this.copy_or_move_clipboard_files(
                                clipboard.files,
                                clipboard.op,
                                &target_dir,
                            ),
                            None => this.set_status("Clipboard does not contain files."),
                        }
                    });
                }
                Err(err) => this.set_status(&format!("Could not read clipboard: {err}")),
            },
        );
    }

    fn copy_or_move_clipboard_files(
        &self,
        files: Vec<gio::File>,
        op: ClipboardOp,
        target_dir: &Path,
    ) {
        if files.is_empty() {
            self.set_status("Clipboard does not contain files.");
            return;
        }

        let target_dir = target_dir.to_path_buf();
        let verb = match op {
            ClipboardOp::Copy => "Copying",
            ClipboardOp::Cut => "Moving",
        };
        let done_verb = match op {
            ClipboardOp::Copy => "Copied",
            ClipboardOp::Cut => "Moved",
        };
        self.run_progress_job(
            verb,
            files.len(),
            move |index| match files.get(index) {
                Some(file) => match op {
                    ClipboardOp::Copy => {
                        copy_dropped_file(file, &target_dir).map_err(|err| err.to_string())
                    }
                    ClipboardOp::Cut => {
                        move_clipboard_file(file, &target_dir).map_err(|err| err.to_string())
                    }
                },
                None => Ok(()),
            },
            {
                let this = self.clone();
                move |outcome, completed, error| match outcome {
                    FileJobOutcome::Finished => {
                        this.reload();
                        if matches!(op, ClipboardOp::Cut) && error.is_none() {
                            this.file_clipboard.borrow_mut().take();
                        }

                        match (completed, error) {
                            (0, Some(err)) => {
                                this.set_status(&format!("Could not paste files: {err}"))
                            }
                            (count, Some(err)) => this.set_status(&format!(
                                "{done_verb} {count} item{}; some items failed: {err}",
                                plural(count)
                            )),
                            (count, None) => this.set_status(&format!(
                                "{done_verb} {count} pasted item{}.",
                                plural(count)
                            )),
                        }
                    }
                    FileJobOutcome::Cancelled => this.set_status(&format!(
                        "Cancelled after {done_verb} {completed} item{}.",
                        plural(completed)
                    )),
                }
            },
        );
    }

    fn watch_file_clipboard_owner(&self) {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };

        let file_clipboard = self.file_clipboard.clone();
        display
            .clipboard()
            .connect_formats_notify(move |clipboard| {
                if !clipboard.is_local() {
                    file_clipboard.borrow_mut().take();
                }
            });
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

        if let Some(item) = self.visible_entries.borrow().get(index as usize) {
            self.path_entry.set_text(&item.display_path());
        }
    }

    fn activate_item(&self, index: i32) {
        if index < 0 {
            return;
        }

        let Some(item) = self.visible_entries.borrow().get(index as usize).cloned() else {
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

    fn selected_indices(&self) -> Vec<i32> {
        let mut indices = match *self.view_mode.borrow() {
            ViewMode::Grid => self
                .grid
                .selected_children()
                .into_iter()
                .map(|child| child.index())
                .filter(|index| *index >= 0)
                .collect::<Vec<_>>(),
            ViewMode::Details | ViewMode::List => self
                .list
                .selected_rows()
                .into_iter()
                .map(|row| row.index())
                .filter(|index| *index >= 0)
                .collect::<Vec<_>>(),
        };

        indices.sort_unstable();
        indices
    }

    fn selected_file_items(&self) -> Vec<FileItem> {
        self.selected_indices()
            .into_iter()
            .filter_map(|index| self.visible_entries.borrow().get(index as usize).cloned())
            .collect()
    }

    fn select_all_visible_items(&self) {
        match *self.view_mode.borrow() {
            ViewMode::Grid => self.grid.select_all(),
            ViewMode::Details | ViewMode::List => self.list.select_all(),
        }
    }

    fn run_progress_job(
        &self,
        title: &str,
        total: usize,
        mut step: impl FnMut(usize) -> Result<(), String> + 'static,
        mut on_finish: impl FnMut(FileJobOutcome, usize, Option<String>) + 'static,
    ) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(title)
            .default_width(420)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.add_css_class("title-3");

        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_fraction(0.0);

        let cancel = gtk::Button::with_label("Cancel");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);

        content.append(&label);
        content.append(&progress);
        content.append(&actions);
        dialog.set_child(Some(&content));
        dialog.present();

        let cancelled = Rc::new(RefCell::new(false));
        let cancelled_for_button = cancelled.clone();
        cancel.connect_clicked(move |_| {
            *cancelled_for_button.borrow_mut() = true;
        });

        let dialog_for_finish = dialog.clone();
        let progress_for_finish = progress.clone();
        let processed = Rc::new(RefCell::new(0usize));
        let processed_for_tick = processed.clone();
        let last_error = Rc::new(RefCell::new(None::<String>));
        let last_error_for_tick = last_error.clone();

        glib::timeout_add_local(std::time::Duration::from_millis(0), move || {
            if *cancelled.borrow() {
                let completed = *processed.borrow();
                on_finish(
                    FileJobOutcome::Cancelled,
                    completed,
                    last_error.borrow().clone(),
                );
                dialog_for_finish.close();
                return ControlFlow::Break;
            }

            let index = *processed_for_tick.borrow();
            if index >= total {
                let completed = *processed_for_tick.borrow();
                on_finish(
                    FileJobOutcome::Finished,
                    completed,
                    last_error_for_tick.borrow().clone(),
                );
                dialog_for_finish.close();
                return ControlFlow::Break;
            }

            if total > 0 {
                progress_for_finish.set_fraction(index as f64 / total as f64);
                progress_for_finish.set_text(Some(&format!("{index} of {total}")));
            }

            match step(index) {
                Ok(()) => {}
                Err(err) => {
                    *last_error_for_tick.borrow_mut() = Some(err);
                }
            }

            *processed_for_tick.borrow_mut() = index + 1;
            ControlFlow::Continue
        });
    }

    fn selected_index_for_action(&self) -> Option<i32> {
        if let Some(index) = self.context_menu_index.borrow_mut().take() {
            if index >= 0 {
                return Some(index);
            }
        }

        self.selected_index()
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

    fn set_search_query(&self, query: String) {
        let normalized = query.trim().to_string();
        if *self.search_query.borrow() == normalized {
            return;
        }

        *self.search_query.borrow_mut() = normalized;
        self.context_menu_index.borrow_mut().take();
        self.render_entries();
    }

    fn set_sort_mode(&self, sort_mode: SortMode) {
        if *self.sort_mode.borrow() == sort_mode {
            return;
        }

        *self.sort_mode.borrow_mut() = sort_mode;
        self.render_entries();
    }

    fn set_sort_descending(&self, descending: bool) {
        if *self.sort_descending.borrow() == descending {
            return;
        }

        *self.sort_descending.borrow_mut() = descending;
        self.render_entries();
    }

    fn show_properties_dialog(&self) {
        let items = self.selected_file_items();
        if items.is_empty() {
            self.set_status("Select one or more items to view properties.");
            return;
        }

        let is_multiple = items.len() > 1;
        let folder_count = items.iter().filter(|item| item.is_dir).count();
        let file_count = items.len().saturating_sub(folder_count);
        let total_size = items.iter().map(|item| item.size).sum::<u64>();
        let title = if is_multiple {
            format!("Properties ({} items)", items.len())
        } else {
            "Properties".to_string()
        };

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(&title)
            .default_width(460)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let header_text = if is_multiple {
            format!("{} items selected", items.len())
        } else {
            items[0].name.clone()
        };
        let header = gtk::Label::new(Some(&header_text));
        header.set_xalign(0.0);
        header.add_css_class("title-3");
        content.append(&header);

        let grid = gtk::Grid::new();
        grid.set_column_spacing(12);
        grid.set_row_spacing(8);

        let mut row = 0;
        let type_text = if is_multiple {
            format!("{folder_count} folders, {file_count} files")
        } else if items[0].is_dir {
            "Folder".to_string()
        } else {
            "File".to_string()
        };
        add_property_row(&grid, &mut row, "Type", type_text);
        let location_text = property_location_text(&items);
        add_property_row(&grid, &mut row, "Location", location_text);
        let size_text = if is_multiple {
            format_size(total_size)
        } else {
            format_size(items[0].size)
        };
        add_property_row(&grid, &mut row, "Size", size_text);
        let modified_text = if is_multiple {
            "Multiple items".to_string()
        } else {
            items[0].modified.clone()
        };
        add_property_row(&grid, &mut row, "Modified", modified_text);

        if let Some(path) = &items[0].path {
            add_property_row(&grid, &mut row, "Path", path.to_string_lossy().into_owned());
        } else {
            add_property_row(&grid, &mut row, "URI", items[0].uri.clone());
        }

        content.append(&grid);

        let close = gtk::Button::with_label("Close");
        close.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&close);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let dialog_for_close = dialog.clone();
        close.connect_clicked(move |_| dialog_for_close.close());
        dialog.present();
    }

    fn show_open_with_dialog(&self) {
        let Some(index) = self.selected_index_for_action() else {
            info!(target: "focaldesk", "show_open_with_dialog: no selected index");
            self.set_status("Select an item to choose an application.");
            return;
        };
        let Some(item) = self.visible_entries.borrow().get(index as usize).cloned() else {
            info!(target: "focaldesk", index, "show_open_with_dialog: no item at index");
            return;
        };
        info!(
            target: "focaldesk",
            name = %item.name,
            content_type = %item.content_type,
            "show_open_with_dialog: building dialog"
        );

        let is_windows_executable = item
            .path
            .as_deref()
            .and_then(Path::extension)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        let mut apps: Vec<gio::AppInfo> = gio::AppInfo::all_for_type(&item.content_type)
            .into_iter()
            // Wine's standard desktop entry is intentionally NoDisplay=true, but it is still
            // the registered handler that belongs in an explicit Open With chooser.
            .filter(|app| is_windows_executable || app.should_show())
            .collect();
        if is_windows_executable && apps.is_empty() {
            if let Some(wine) = gio::DesktopAppInfo::new("wine.desktop") {
                apps.push(wine.upcast());
            }
        }
        info!(target: "focaldesk", app_count = apps.len(), "show_open_with_dialog: apps found");

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(format!("Open With - {}", item.name))
            .default_width(380)
            .default_height(420)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.set_vexpand(true);

        let header = gtk::Label::new(Some(&format!(
            "Choose an application to open \"{}\"",
            item.name
        )));
        header.set_xalign(0.0);
        header.set_wrap(true);
        content.append(&header);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");

        if apps.is_empty() {
            let empty = gtk::Label::new(Some("No applications are available for this file type."));
            empty.add_css_class("dim-label");
            empty.set_margin_top(8);
            empty.set_margin_bottom(8);
            content.append(&empty);
        } else {
            for app in &apps {
                let row = gtk::ListBoxRow::new();
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row_box.set_margin_top(6);
                row_box.set_margin_bottom(6);
                row_box.set_margin_start(8);
                row_box.set_margin_end(8);

                let icon = match app.icon() {
                    Some(gicon) => gtk::Image::from_gicon(&gicon),
                    None => gtk::Image::from_icon_name("application-x-executable-symbolic"),
                };
                icon.set_pixel_size(24);
                row_box.append(&icon);

                let name = gtk::Label::new(Some(&app.name()));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                row_box.append(&name);

                row.set_child(Some(&row_box));
                list.append(&row);
            }
            list.select_row(list.row_at_index(0).as_ref());
        }

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_min_content_height(200);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&list));
        content.append(&scroller);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let set_default = gtk::Button::with_label("Set as Default");
        let open_once = gtk::Button::with_label("Open Once");
        open_once.add_css_class("suggested-action");
        set_default.set_sensitive(!apps.is_empty());
        open_once.set_sensitive(!apps.is_empty());
        actions.append(&cancel);
        actions.append(&set_default);
        actions.append(&open_once);
        content.append(&actions);

        dialog.set_child(Some(&content));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());

        let key_controller = gtk::EventControllerKey::new();
        let dialog_for_escape = dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                dialog_for_escape.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);

        let launch = {
            let dialog = dialog.clone();
            let list = list.clone();
            let apps = apps.clone();
            let item = item.clone();
            let this = self.clone();
            move |make_default: bool| {
                let Some(row) = list.selected_row() else {
                    return;
                };
                let index = row.index();
                if index < 0 {
                    return;
                }
                let Some(app) = apps.get(index as usize) else {
                    return;
                };

                let force_x11 = item
                    .path
                    .as_deref()
                    .and_then(Path::extension)
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
                if make_default {
                    let association_type = if force_x11 {
                        "application/x-ms-dos-executable"
                    } else {
                        &item.content_type
                    };
                    if let Err(err) = app.set_as_default_for_type(association_type) {
                        this.set_status(&format!("Could not set default application: {err}"));
                        return;
                    }
                }

                let result = if let Some(display) = gtk::gdk::Display::default() {
                    let context = display.app_launch_context();
                    apply_launch_environment(&context, force_x11);
                    app.launch(std::slice::from_ref(&item.file), Some(&context))
                } else {
                    let context = gio::AppLaunchContext::new();
                    apply_launch_environment(&context, force_x11);
                    app.launch(std::slice::from_ref(&item.file), Some(&context))
                };
                if let Err(err) = result {
                    this.set_status(&format!("Could not launch {}: {err}", app.name()));
                }
                dialog.close();
            }
        };

        let launch_once = launch.clone();
        open_once.connect_clicked(move |_| launch_once(false));
        let launch_default = launch.clone();
        set_default.connect_clicked(move |_| launch_default(true));
        list.connect_row_activated(move |_, _| launch(false));

        info!(target: "focaldesk", "show_open_with_dialog: presenting");
        dialog.present();
    }

    fn install_drop_target(&self, widget: &impl IsA<gtk::Widget>) {
        let target = gtk::DropTargetAsync::new(None, gtk::gdk::DragAction::COPY);

        target.connect_accept(|_, drop| {
            let formats = drop.formats();
            formats.contains_type(gtk::gdk::FileList::static_type())
                || FILE_TRANSFER_MIME_TYPES
                    .iter()
                    .any(|mime_type| formats.contain_mime_type(mime_type))
        });

        let this = self.clone();
        target.connect_drop(move |_, drop, _, _| {
            let this = this.clone();
            let drop_for_fallback = drop.clone();
            drop.read_value_async(
                gtk::gdk::FileList::static_type(),
                glib::Priority::default(),
                gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file_list) = result.and_then(|value| {
                        value.get::<gtk::gdk::FileList>().map_err(|_| {
                            glib::Error::new(gio::IOErrorEnum::InvalidData, "not files")
                        })
                    }) {
                        let success = this.copy_dropped_files(file_list.files());
                        drop_for_fallback.finish(if success {
                            gtk::gdk::DragAction::COPY
                        } else {
                            gtk::gdk::DragAction::empty()
                        });
                        return;
                    }

                    this.read_dropped_mime_files(drop_for_fallback);
                },
            );
            true
        });

        widget.add_controller(target);
    }

    fn read_dropped_mime_files(&self, drop: gtk::gdk::Drop) {
        let this = self.clone();
        let drop_for_read = drop.clone();
        drop_for_read.read_async(
            FILE_TRANSFER_MIME_TYPES,
            glib::Priority::default(),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok((stream, mime_type)) => {
                    let drop = drop.clone();
                    let this = this.clone();
                    read_transfer_text(stream, move |text| {
                        let success = file_clipboard_from_text(mime_type.as_str(), &text)
                            .map(|clipboard| this.copy_dropped_files(clipboard.files))
                            .unwrap_or_else(|| {
                                this.set_status("Drop did not contain files.");
                                false
                            });
                        drop.finish(if success {
                            gtk::gdk::DragAction::COPY
                        } else {
                            gtk::gdk::DragAction::empty()
                        });
                    });
                }
                Err(err) => {
                    this.set_status(&format!("Could not read dropped files: {err}"));
                    drop.finish(gtk::gdk::DragAction::empty());
                }
            },
        );
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
        self.run_progress_job(
            "Copying",
            files.len(),
            move |index| match files.get(index) {
                Some(file) => copy_dropped_file(file, &target_dir).map_err(|err| err.to_string()),
                None => Ok(()),
            },
            {
                let this = self.clone();
                move |outcome, completed, error| match outcome {
                    FileJobOutcome::Finished => {
                        this.reload();
                        match (completed, error) {
                            (0, Some(err)) => {
                                this.set_status(&format!("Could not copy dropped files: {err}"));
                            }
                            (count, Some(err)) => {
                                this.set_status(&format!(
                                    "Copied {count} item{}; some items failed: {err}",
                                    plural(count)
                                ));
                            }
                            (count, None) => {
                                this.set_status(&format!(
                                    "Copied {count} dropped item{}.",
                                    plural(count)
                                ));
                            }
                        }
                    }
                    FileJobOutcome::Cancelled => {
                        this.set_status(&format!(
                            "Cancelled after copying {completed} item{}.",
                            plural(completed)
                        ));
                    }
                }
            },
        );
        true
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

fn local_directory_revision(location: &Location) -> Option<(std::time::SystemTime, usize)> {
    let Location::Path(path) = location else {
        return None;
    };
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let entry_count = std::fs::read_dir(path).ok()?.filter_map(Result::ok).count();
    Some((modified, entry_count))
}

fn read_file_items(file: &gio::File, show_hidden: bool) -> Result<Vec<FileItem>, glib::Error> {
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::size,time::modified,\
         standard::icon,standard::symbolic-icon,standard::content-type",
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
        let content_type = info
            .content_type()
            .map(|value| value.to_string())
            .unwrap_or_else(|| {
                if is_dir {
                    "inode/directory".to_string()
                } else {
                    "application/octet-stream".to_string()
                }
            });
        let icon = info
            .symbolic_icon()
            .or_else(|| info.icon())
            .unwrap_or_else(|| gio::Icon::from(gio::ThemedIcon::new("text-x-generic-symbolic")));
        items.push(FileItem {
            name,
            path: child.path(),
            uri: child.uri().to_string(),
            file: child,
            is_dir,
            size: info.size().max(0) as u64,
            modified: modified_text(&info),
            content_type,
            icon,
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

    let icon = gtk::Image::from_gicon(&item.icon);
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

    let icon = gtk::Image::from_gicon(&item.icon);
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

    let icon = gtk::Image::from_gicon(&item.icon);
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
    rename_item: impl Fn() + 'static,
) {
    let rename_item: Rc<dyn Fn()> = Rc::new(rename_item);
    let click = gtk::GestureClick::new();
    click.set_button(3);

    click.connect_pressed(move |gesture, _, x, y| {
        select_item();
        if let Some(parent) = gesture.widget() {
            let popover = gtk::Popover::new();
            let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
            menu.set_margin_top(6);
            menu.set_margin_bottom(6);
            menu.set_margin_start(6);
            menu.set_margin_end(6);

            menu.append(&file_context_action_button("Open", "win.file-open"));
            if matches!(target, FileContextTarget::Folder) {
                menu.append(&file_context_action_button(
                    "Open in New Tab",
                    "win.file-open-tab",
                ));
            }
            menu.append(&file_context_action_button(
                "Open With...",
                "win.file-open-with",
            ));
            menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            menu.append(&file_context_action_button("Cut\tCtrl+X", "win.file-cut"));
            menu.append(&file_context_action_button("Copy\tCtrl+C", "win.file-copy"));
            menu.append(&file_context_action_button(
                "Move to...",
                "win.file-move-to",
            ));
            menu.append(&file_context_action_button(
                "Copy to...",
                "win.file-copy-to",
            ));
            menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

            let rename = gtk::Button::with_label("Rename...\tF2");
            rename.add_css_class("flat");
            rename.set_halign(gtk::Align::Fill);
            let rename_item = rename_item.clone();
            let popover_for_rename = popover.clone();
            rename.connect_clicked(move |_| {
                popover_for_rename.popdown();
                rename_item();
            });
            menu.append(&rename);

            if matches!(target, FileContextTarget::Folder) {
                menu.append(&file_context_action_button(
                    "Paste Into Folder",
                    "win.file-paste-into-folder",
                ));
            }
            menu.append(&file_context_action_button(
                "Compress...",
                "win.file-compress",
            ));
            menu.append(&file_context_action_button(
                "Move to Trash\tDelete",
                "win.file-move-to-trash",
            ));
            menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            menu.append(&file_context_action_button(
                "Properties\tAlt+Return",
                "win.file-properties",
            ));

            popover.set_child(Some(&menu));
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

fn file_context_action_button(label: &str, action: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("flat");
    button.set_halign(gtk::Align::Fill);
    button.set_action_name(Some(action));
    button.connect_clicked(|button| {
        // GtkPopoverMenu closes itself after activating a menu item, but these
        // custom action buttons live in a plain GtkPopover. Close it on the next
        // main-loop turn so the action resolves through the current widget tree
        // first and any modal dialog it opens can receive input normally.
        let button = button.clone();
        glib::idle_add_local_once(move || {
            let mut ancestor = button.parent();
            while let Some(widget) = ancestor {
                if let Ok(popover) = widget.clone().downcast::<gtk::Popover>() {
                    popover.popdown();
                    break;
                }
                ancestor = widget.parent();
            }
        });
    });
    button
}

fn attach_file_drag_source(widget: &impl IsA<gtk::Widget>, item: FileItem) {
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::COPY);

    source.connect_prepare(move |_, _, _| {
        let files = vec![item.file.clone()];
        Some(file_clipboard_provider(&files, ClipboardOp::Copy))
    });

    widget.add_controller(source);
}

fn file_clipboard_provider(files: &[gio::File], op: ClipboardOp) -> gtk::gdk::ContentProvider {
    let uri_list = uri_list_text(files);
    let uri_provider = gtk::gdk::ContentProvider::for_bytes(
        "text/uri-list",
        &glib::Bytes::from_owned(uri_list.clone().into_bytes()),
    );

    let gnome_files = gnome_copied_files_text(files, op);
    let gnome_provider = gtk::gdk::ContentProvider::for_bytes(
        "x-special/gnome-copied-files",
        &glib::Bytes::from_owned(gnome_files.into_bytes()),
    );

    let file_list = gtk::gdk::FileList::from_array(files);
    let file_provider = gtk::gdk::ContentProvider::for_value(&file_list.to_value());

    let text_provider = gtk::gdk::ContentProvider::for_value(&uri_list.to_value());

    gtk::gdk::ContentProvider::new_union(&[
        uri_provider,
        gnome_provider,
        file_provider,
        text_provider,
    ])
}

fn uri_list_text(files: &[gio::File]) -> String {
    files
        .iter()
        .map(|file| file.uri().to_string())
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

fn gnome_copied_files_text(files: &[gio::File], op: ClipboardOp) -> String {
    let action = match op {
        ClipboardOp::Copy => "copy",
        ClipboardOp::Cut => "cut",
    };
    format!("{action}\n{}", uri_list_text(files).replace("\r\n", "\n"))
}

fn files_from_uri_text(text: &str) -> Vec<gio::File> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('#'))
        .filter(|line| *line != "copy" && *line != "cut")
        .filter(|line| line.contains(':'))
        .map(gio::File::for_uri)
        .collect()
}

fn file_clipboard_from_text(mime_type: &str, text: &str) -> Option<FileClipboard> {
    if mime_type == "x-special/gnome-copied-files" {
        let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
        let op = match lines.next() {
            Some("cut") => ClipboardOp::Cut,
            Some("copy") => ClipboardOp::Copy,
            _ => ClipboardOp::Copy,
        };
        let files = lines
            .filter(|line| !line.starts_with('#'))
            .filter(|line| line.contains(':'))
            .map(gio::File::for_uri)
            .collect::<Vec<_>>();
        return (!files.is_empty()).then_some(FileClipboard { files, op });
    }

    let files = files_from_uri_text(text);
    (!files.is_empty()).then_some(FileClipboard {
        files,
        op: ClipboardOp::Copy,
    })
}

fn read_transfer_text(stream: gio::InputStream, callback: impl FnOnce(String) + 'static) {
    stream.read_bytes_async(
        1024 * 1024,
        glib::Priority::default(),
        gio::Cancellable::NONE,
        move |result| {
            let text = result
                .ok()
                .and_then(|bytes| String::from_utf8(bytes.as_ref().to_vec()).ok())
                .unwrap_or_default();
            callback(text);
        },
    );
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
        attach_sidebar_context_menu(&row, sidebar_kind_for_place(path), path);
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
        .map_err(|err| io::Error::other(err.to_string()))
    }
}

fn move_clipboard_file(file: &gio::File, target_dir: &Path) -> io::Result<()> {
    let name = file.basename().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard file has no basename",
        )
    })?;
    let destination = available_destination(target_dir, Path::new(&name));

    if let Some(source) = file.path() {
        if source == destination {
            return Ok(());
        }
        if source.is_dir() && is_path_inside(target_dir, &source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot move a folder into itself",
            ));
        }
        std::fs::rename(&source, &destination).or_else(|_| {
            copy_path_recursively(&source, &destination)?;
            remove_path_recursively(&source)
        })
    } else {
        let destination_file = gio::File::for_path(&destination);
        file.move_(
            &destination_file,
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|err| io::Error::other(err.to_string()))
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

fn remove_path_recursively(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
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

fn create_tar_gz_archive(sources: &[PathBuf], destination: &Path) -> io::Result<()> {
    if sources.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no files were selected",
        ));
    }
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the destination archive already exists",
        ));
    }

    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the archive has no parent folder",
        )
    })?;
    let archive_name = destination.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the archive has no file name")
    })?;

    let mut source_names = Vec::with_capacity(sources.len());
    for source in sources {
        if source.parent() != Some(parent) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "selected items must be in the archive's folder",
            ));
        }
        source_names.push(source.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "a selected item has no name")
        })?);
    }

    let output = Command::new("tar")
        .current_dir(parent)
        .arg("--create")
        .arg("--gzip")
        .arg("--file")
        .arg(archive_name)
        .arg("--")
        .args(source_names)
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(io::Error::other(if stderr.is_empty() {
        format!("tar exited with {}", output.status)
    } else {
        stderr
    }))
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

    app_info
        .launch(&[], Some(&context))
        .map_err(|err| io::Error::other(format!("desktop entry launch failed: {err}")))?;

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

fn tab_title_for_location(location: &Location) -> String {
    match location {
        Location::Path(path) if *path == home_dir() => "Home".to_string(),
        Location::Path(path) => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        Location::Trash => "Trash".to_string(),
        Location::Uri(uri) => uri.clone(),
        Location::Separator => "-".to_string(),
    }
}

fn sidebar_location_from_parameter(parameter: Option<&glib::Variant>) -> Option<Location> {
    parameter
        .and_then(|parameter| parameter.get::<String>())
        .map(|path| {
            if path == "trash:///" {
                Location::Trash
            } else {
                Location::Path(PathBuf::from(path))
            }
        })
}

fn open_sidebar_location_in_new_window(location: &Location) -> io::Result<()> {
    let Location::Path(path) = location else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only local folders can be opened in a new window",
        ));
    };

    Command::new(std::env::current_exe()?).arg(path).spawn()?;
    Ok(())
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

fn file_matches_search(item: &FileItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let query = query.to_ascii_lowercase();
    item.name.to_ascii_lowercase().contains(&query)
        || item.uri.to_ascii_lowercase().contains(&query)
        || item
            .path
            .as_ref()
            .is_some_and(|path| path.to_string_lossy().to_ascii_lowercase().contains(&query))
}

fn sort_mode_from_dropdown(index: u32) -> SortMode {
    match index {
        1 => SortMode::Size,
        2 => SortMode::Type,
        3 => SortMode::Modified,
        _ => SortMode::Name,
    }
}

fn sort_file_items(items: &mut [FileItem], mode: SortMode, descending: bool) {
    items.sort_by(|a, b| {
        let dir_cmp = b.is_dir.cmp(&a.is_dir);
        if dir_cmp != std::cmp::Ordering::Equal {
            return dir_cmp;
        }

        let ordering = match mode {
            SortMode::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortMode::Size => a
                .size
                .cmp(&b.size)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            SortMode::Type => file_sort_key(a)
                .cmp(&file_sort_key(b))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            SortMode::Modified => a
                .modified
                .cmp(&b.modified)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        };

        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

fn file_sort_key(item: &FileItem) -> String {
    if item.is_dir {
        "folder".to_string()
    } else {
        item.path
            .as_ref()
            .and_then(|path| path.extension())
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_else(|| "file".to_string())
    }
}

fn property_location_text(items: &[FileItem]) -> String {
    let mut locations = items
        .iter()
        .map(|item| {
            item.path
                .as_ref()
                .and_then(|path| {
                    path.parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| item.uri.clone())
        })
        .collect::<Vec<_>>();
    locations.sort();
    locations.dedup();

    match locations.as_slice() {
        [single] => single.clone(),
        _ => "Multiple locations".to_string(),
    }
}

fn add_property_row(grid: &gtk::Grid, row: &mut i32, label: &str, value: impl AsRef<str>) {
    let key = gtk::Label::new(Some(label));
    key.set_xalign(0.0);
    key.add_css_class("dim-label");

    let value = gtk::Label::new(Some(value.as_ref()));
    value.set_xalign(0.0);
    value.set_wrap(true);
    value.set_selectable(true);

    grid.attach(&key, 0, *row, 1, 1);
    grid.attach(&value, 1, *row, 1, 1);
    *row += 1;
}

fn sidebar_kind_for_place(path: &str) -> SidebarKind {
    match path {
        "trash:///" => SidebarKind::Trash,
        "-" => SidebarKind::Separator,
        _ => SidebarKind::Folder,
    }
}

fn attach_sidebar_context_menu(widget: &impl IsA<gtk::Widget>, kind: SidebarKind, path: &str) {
    if matches!(kind, SidebarKind::Separator) {
        return;
    }

    let menu = gio::Menu::new();
    let target = path.to_variant();

    menu.append_item(&sidebar_menu_item("Open", "win.sidebar-open", &target));

    if matches!(kind, SidebarKind::Folder) {
        menu.append_item(&sidebar_menu_item(
            "Open in New Tab",
            "win.sidebar-open-tab",
            &target,
        ));
        menu.append_item(&sidebar_menu_item(
            "Open in New Window",
            "win.sidebar-open-window",
            &target,
        ));
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
        section.append_item(&sidebar_menu_item(
            "Properties",
            "win.sidebar-properties",
            &target,
        ));
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

fn sidebar_menu_item(label: &str, action: &str, target: &glib::Variant) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(target));
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "focaldesk-files-{label}-test-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn directory_revision_changes_when_file_is_created() {
        let root = unique_test_dir("revision");
        std::fs::create_dir_all(&root).expect("create test folder");
        let location = Location::Path(root.clone());
        let before = local_directory_revision(&location).expect("initial revision");

        std::fs::write(root.join("downloaded.txt"), "new file").expect("write test file");

        let after = local_directory_revision(&location).expect("updated revision");
        assert_ne!(before, after);
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn creates_tar_gz_archive_with_selected_items() {
        let root = unique_test_dir("archive");
        std::fs::create_dir_all(root.join("folder")).expect("create test folder");
        std::fs::write(root.join("note.txt"), "archive me").expect("write test file");
        std::fs::write(root.join("folder/nested.txt"), "nested").expect("write nested file");

        let destination = root.join("selection.tar.gz");
        create_tar_gz_archive(&[root.join("note.txt"), root.join("folder")], &destination)
            .expect("create archive");

        let listing = Command::new("tar")
            .arg("--list")
            .arg("--gzip")
            .arg("--file")
            .arg(&destination)
            .output()
            .expect("list archive");
        assert!(listing.status.success());
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.lines().any(|line| line == "note.txt"));
        assert!(listing.lines().any(|line| line == "folder/nested.txt"));

        std::fs::remove_dir_all(root).expect("remove test folder");
    }
}
