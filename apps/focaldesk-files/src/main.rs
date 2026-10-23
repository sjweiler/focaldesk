use adw::prelude::*;
use focaldesk_config::load_config;
use focaldesk_gtk::{classify_status, StateKind, StatusBanner, ToastOverlay};
use focaldesk_launcher_state::{is_file_favorite, remember_recent_file, toggle_file_favorite};
use focaldesk_logging::{init_default_logging, session_id};
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{gtk_app_css, gtk_app_prefers_dark, theme_by_name, GtkAppThemeOptions};
use glib::ControlFlow;
use gtk::gio;
use gtk::gio::prelude::{AppInfoExt, MountExt, VolumeExt, VolumeMonitorExt};
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use tracing::info;

mod archive;
mod thumbnails;
mod transfer;

use archive::{archive_kind, default_extract_name, extract_archive, ExtractionReport};
use thumbnails::{
    generate_thumbnail, is_text_preview, prune_cache, read_text_preview, thumbnail_request,
    ThumbnailRequest,
};
use transfer::{
    remove_path, run_transfer, ConflictPolicy, TransferEvent, TransferOperation, TransferRequest,
    TransferUndo,
};

#[derive(Debug, Clone)]
struct FileItem {
    name: String,
    relative_path: Option<String>,
    trash_original_path: Option<PathBuf>,
    file: gio::File,
    path: Option<PathBuf>,
    uri: String,
    is_dir: bool,
    size: u64,
    modified: String,
    content_type: String,
    icon: gio::Icon,
}

impl FileItem {
    fn display_name(&self) -> &str {
        self.relative_path.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug)]
struct SearchMatch {
    path: PathBuf,
    relative_path: String,
    is_dir: bool,
    size: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Debug, Default)]
struct RecursiveSearchOutput {
    matches: Vec<SearchMatch>,
    scanned_dirs: usize,
    skipped_dirs: usize,
    result_limit_reached: bool,
    directory_limit_reached: bool,
    depth_limit_reached: bool,
    cancelled: bool,
}

#[derive(Debug, Clone, Copy)]
struct SearchLimits {
    max_results: usize,
    max_directories: usize,
    max_depth: usize,
}

#[derive(Debug, Clone)]
struct RecursiveSearchView {
    items: Vec<FileItem>,
    scanned_dirs: usize,
    skipped_dirs: usize,
    result_limit_reached: bool,
    directory_limit_reached: bool,
    depth_limit_reached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarKind {
    Folder,
    Projects,
    Trash,
    Separator,
    UserBookmark { available: bool },
    MountedDevice { can_eject: bool },
    UnmountedVolume { can_eject: bool },
    NetworkBookmark,
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

#[derive(Clone)]
enum UndoStep {
    MoveBack {
        from: PathBuf,
        to: gio::File,
    },
    BulkRename {
        moves: Vec<(PathBuf, PathBuf)>,
    },
    RemoveCreated {
        path: PathBuf,
        empty_only: bool,
    },
    RemoveExtracted {
        paths: Vec<PathBuf>,
    },
    RestoreReplacedCopy {
        destination: PathBuf,
        backup: PathBuf,
    },
    RestoreReplacedMove {
        source: PathBuf,
        destination: PathBuf,
        backup: PathBuf,
    },
    RestorePermissions {
        modes: Vec<(PathBuf, u32)>,
    },
    RestoreTrash {
        original: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkRenameMode {
    FindReplace,
    Prefix,
    Suffix,
    Number,
    Lowercase,
    Uppercase,
}

#[derive(Debug, Clone)]
struct BulkRenameOptions {
    mode: BulkRenameMode,
    primary: String,
    secondary: String,
    preserve_extension: bool,
}

#[derive(Debug, Clone)]
struct BulkRenamePlan {
    mappings: Vec<(String, String)>,
    moves: Vec<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone)]
struct PermissionSnapshot {
    path: PathBuf,
    mode: u32,
    uid: u32,
    gid: u32,
    device: u64,
    inode: u64,
    is_dir: bool,
    is_file: bool,
}

#[derive(Debug, Clone, Copy)]
struct PermissionChanges {
    bits: [Option<bool>; 9],
}

#[derive(Debug)]
struct PermissionChangeReport {
    previous_modes: Vec<(PathBuf, u32)>,
    changed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalBookmark {
    uri: String,
    path: PathBuf,
    label: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum BookmarkDirection {
    Up,
    Down,
}

#[derive(Clone)]
struct UndoRecord {
    description: String,
    steps: Vec<UndoStep>,
}

#[derive(Debug, Clone)]
struct FileClipboard {
    files: Vec<gio::File>,
    op: ClipboardOp,
}

struct DeviceMonitorSubscription {
    monitor: gio::VolumeMonitor,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl DeviceMonitorSubscription {
    fn new() -> Self {
        Self {
            monitor: gio::VolumeMonitor::get(),
            handlers: RefCell::new(Vec::new()),
        }
    }
}

impl Drop for DeviceMonitorSubscription {
    fn drop(&mut self) {
        for handler in self.handlers.get_mut().drain(..) {
            self.monitor.disconnect(handler);
        }
    }
}

const FILE_TRANSFER_MIME_TYPES: &[&str] = &["x-special/gnome-copied-files", "text/uri-list"];
const DIRECTORY_RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
const DIRECTORY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_UNDO_RECORDS: usize = 20;
const MAX_NETWORK_BOOKMARKS: usize = 64;
const MAX_NETWORK_URI_BYTES: usize = 2_048;
const MAX_LOCAL_BOOKMARKS: usize = 256;
const MAX_BOOKMARK_LINES: usize = 1_024;
const MAX_BOOKMARK_FILE_BYTES: u64 = 1024 * 1024;
const MAX_BOOKMARK_LINE_BYTES: usize = 8 * 1024;
const MAX_BOOKMARK_LABEL_BYTES: usize = 256;
const MAX_PROJECTS_SETTINGS_BYTES: u64 = 16 * 1024;
const MAX_BULK_RENAME_ITEMS: usize = 500;
const MAX_PERMISSION_ITEMS: usize = 500;
const MAX_RECURSIVE_PERMISSION_ITEMS: usize = 10_000;
const PERMISSION_MASKS: [u32; 9] = [
    0o400, 0o040, 0o004, 0o200, 0o020, 0o002, 0o100, 0o010, 0o001,
];
const RECURSIVE_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
const RECURSIVE_SEARCH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const RECURSIVE_SEARCH_LIMITS: SearchLimits = SearchLimits {
    max_results: 500,
    max_directories: 10_000,
    max_depth: 32,
};

#[derive(Debug, Default, Deserialize, Serialize)]
struct FilesSidebarSettings {
    projects_location: Option<PathBuf>,
}

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
    status: StatusBanner,
    toasts: ToastOverlay,
    hidden_toggle: gtk::ToggleButton,
    places: gtk::StringList,
    device_monitor: Rc<DeviceMonitorSubscription>,
    view_mode: Rc<RefCell<ViewMode>>,
    sort_mode: Rc<RefCell<SortMode>>,
    sort_descending: Rc<RefCell<bool>>,
    search_query: Rc<RefCell<String>>,
    recursive_search_enabled: Rc<RefCell<bool>>,
    recursive_search_result: Rc<RefCell<Option<RecursiveSearchView>>>,
    search_generation: Arc<AtomicU64>,
    pending_search: Rc<RefCell<Option<glib::SourceId>>>,
    current_location: Rc<RefCell<Location>>,
    entries: Rc<RefCell<Vec<FileItem>>>,
    visible_entries: Rc<RefCell<Vec<FileItem>>>,
    file_clipboard: Rc<RefCell<Option<FileClipboard>>>,
    undo_history: Rc<RefCell<VecDeque<UndoRecord>>>,
    pending_toast_undo: Rc<RefCell<Option<String>>>,
    back_stack: Rc<RefCell<Vec<Location>>>,
    forward_stack: Rc<RefCell<Vec<Location>>>,
    context_menu_index: Rc<RefCell<Option<i32>>>,
    tab_page: Rc<RefCell<Option<gtk::StackPage>>>,
    directory_monitor: Rc<RefCell<Option<gio::FileMonitor>>>,
    pending_directory_reload: Rc<RefCell<Option<glib::SourceId>>>,
    directory_poll: Rc<RefCell<Option<glib::SourceId>>>,
    directory_revision: Rc<RefCell<Option<(std::time::SystemTime, usize)>>>,
    preview_picture: gtk::Picture,
    preview_title: gtk::Label,
    preview_metadata: gtk::Label,
    preview_text: gtk::Label,
    thumbnail_generation: Arc<AtomicU64>,
    preview_generation: Arc<AtomicU64>,
}

struct GridThumbnailJob {
    request: ThumbnailRequest,
    stack: gtk::Stack,
    picture: gtk::Picture,
}

enum PreviewResult {
    Picture(PathBuf),
    Text(String),
    Unavailable,
}

#[derive(Clone)]
struct TabManager {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    tabs: Rc<RefCell<Vec<FileManager>>>,
    tab_counter: Rc<RefCell<u32>>,
    toasts: ToastOverlay,
    pending_toast_undo: Rc<RefCell<Option<String>>>,
}

impl TabManager {
    fn new(window: &adw::ApplicationWindow, stack: &gtk::Stack, toasts: &ToastOverlay) -> Self {
        Self {
            window: window.clone(),
            stack: stack.clone(),
            tabs: Rc::new(RefCell::new(Vec::new())),
            tab_counter: Rc::new(RefCell::new(1)),
            toasts: toasts.clone(),
            pending_toast_undo: Rc::new(RefCell::new(None)),
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

    fn reload_places(&self) {
        for tab in self.tabs.borrow().iter() {
            tab.load_places();
        }
    }

    fn add_folder_bookmark(&self, path: &Path) {
        match add_local_bookmark(path) {
            Ok(true) => {
                self.reload_places();
                self.with_active_tab(|tab| tab.set_status("Added folder to Places."));
            }
            Ok(false) => {
                self.with_active_tab(|tab| tab.set_status("This folder is already in Places."));
            }
            Err(error) => self.with_active_tab(|tab| {
                tab.set_status(&format!("Could not add folder to Places: {error}"));
            }),
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

        let projects_location = gio::SimpleAction::new("projects-location", None);
        let this = self.clone();
        projects_location.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.choose_projects_location());
        });
        self.window.add_action(&projects_location);

        let empty_trash = gio::SimpleAction::new("empty-trash", None);
        let this = self.clone();
        empty_trash.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.show_empty_trash_confirmation());
        });
        self.window.add_action(&empty_trash);

        let properties =
            gio::SimpleAction::new("sidebar-properties", Some(&String::static_variant_type()));
        let this = self.clone();
        properties.connect_activate(move |_, parameter| {
            let Some(location) = sidebar_location_from_parameter(parameter) else {
                return;
            };
            this.with_active_tab(|tab| tab.show_location_properties_dialog(&location));
        });
        self.window.add_action(&properties);

        let device_open =
            gio::SimpleAction::new("device-open", Some(&String::static_variant_type()));
        let this = self.clone();
        device_open.connect_activate(move |_, parameter| {
            let Some(key) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.with_active_tab(|tab| tab.mount_volume_and_open(&key));
        });
        self.window.add_action(&device_open);

        let device_unmount =
            gio::SimpleAction::new("device-unmount", Some(&String::static_variant_type()));
        let this = self.clone();
        device_unmount.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.with_active_tab(|tab| tab.unmount_device(&uri, false));
        });
        self.window.add_action(&device_unmount);

        let device_eject_mount =
            gio::SimpleAction::new("device-eject-mount", Some(&String::static_variant_type()));
        let this = self.clone();
        device_eject_mount.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.with_active_tab(|tab| tab.unmount_device(&uri, true));
        });
        self.window.add_action(&device_eject_mount);

        let device_eject_volume =
            gio::SimpleAction::new("device-eject-volume", Some(&String::static_variant_type()));
        let this = self.clone();
        device_eject_volume.connect_activate(move |_, parameter| {
            let Some(key) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.with_active_tab(|tab| tab.eject_volume(&key));
        });
        self.window.add_action(&device_eject_volume);

        let connect_server = gio::SimpleAction::new("connect-server", None);
        let this = self.clone();
        connect_server.connect_activate(move |_, _| this.show_connect_to_server_dialog());
        self.window.add_action(&connect_server);

        let network_open =
            gio::SimpleAction::new("network-open", Some(&String::static_variant_type()));
        let this = self.clone();
        network_open.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.with_active_tab(|tab| tab.connect_network_location(&uri));
        });
        self.window.add_action(&network_open);

        let network_bookmark_remove = gio::SimpleAction::new(
            "network-bookmark-remove",
            Some(&String::static_variant_type()),
        );
        let this = self.clone();
        network_bookmark_remove.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            match remove_network_bookmark(&uri) {
                Ok(true) => {
                    for tab in this.tabs.borrow().iter() {
                        tab.load_places();
                    }
                    this.with_active_tab(|tab| tab.set_status("Removed server bookmark."));
                }
                Ok(false) => {
                    this.with_active_tab(|tab| {
                        tab.set_status("Server bookmark was already removed.")
                    });
                }
                Err(err) => this.with_active_tab(|tab| {
                    tab.set_status(&format!("Could not remove server bookmark: {err}"));
                }),
            }
        });
        self.window.add_action(&network_bookmark_remove);

        let bookmark_current = gio::SimpleAction::new("bookmark-current", None);
        let this = self.clone();
        bookmark_current.connect_activate(move |_, _| {
            let Some(tab) = this.active_tab() else {
                return;
            };
            let location = tab.current_location.borrow().clone();
            match location {
                Location::Path(path) => this.add_folder_bookmark(&path),
                _ => tab.set_status("Only local folders can be added to Places."),
            }
        });
        self.window.add_action(&bookmark_current);

        let bookmark_selected = gio::SimpleAction::new("file-add-bookmark", None);
        let this = self.clone();
        bookmark_selected.connect_activate(move |_, _| {
            let Some(tab) = this.active_tab() else {
                return;
            };
            let selected = tab.selected_file_items();
            match selected.as_slice() {
                [item] if item.is_dir => match &item.path {
                    Some(path) => this.add_folder_bookmark(path),
                    None => tab.set_status("Only local folders can be added to Places."),
                },
                _ => tab.set_status("Select one local folder to add to Places."),
            }
        });
        self.window.add_action(&bookmark_selected);

        let bookmark_open =
            gio::SimpleAction::new("bookmark-open", Some(&String::static_variant_type()));
        let this = self.clone();
        bookmark_open.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            let Some(path) = local_bookmark_path(&uri) else {
                return;
            };
            this.with_active_tab(|tab| {
                if path.is_dir() {
                    tab.open_location(Location::Path(path.clone()), true);
                } else {
                    tab.set_status("That bookmarked folder is currently unavailable.");
                }
            });
        });
        self.window.add_action(&bookmark_open);

        let bookmark_rename =
            gio::SimpleAction::new("bookmark-rename", Some(&String::static_variant_type()));
        let this = self.clone();
        bookmark_rename.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            this.show_bookmark_rename_dialog(uri);
        });
        self.window.add_action(&bookmark_rename);

        for (action_name, direction) in [
            ("bookmark-move-up", BookmarkDirection::Up),
            ("bookmark-move-down", BookmarkDirection::Down),
        ] {
            let action = gio::SimpleAction::new(action_name, Some(&String::static_variant_type()));
            let this = self.clone();
            action.connect_activate(move |_, parameter| {
                let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                    return;
                };
                match move_local_bookmark(&uri, direction) {
                    Ok(true) => {
                        this.reload_places();
                        this.with_active_tab(|tab| tab.set_status("Reordered Places bookmark."));
                    }
                    Ok(false) => this.with_active_tab(|tab| {
                        tab.set_status("That bookmark is already at the edge of the list.");
                    }),
                    Err(error) => this.with_active_tab(|tab| {
                        tab.set_status(&format!("Could not reorder bookmark: {error}"));
                    }),
                }
            });
            self.window.add_action(&action);
        }

        let bookmark_remove =
            gio::SimpleAction::new("bookmark-remove", Some(&String::static_variant_type()));
        let this = self.clone();
        bookmark_remove.connect_activate(move |_, parameter| {
            let Some(uri) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            match remove_local_bookmark(&uri) {
                Ok(true) => {
                    this.reload_places();
                    this.with_active_tab(|tab| tab.set_status("Removed folder from Places."));
                }
                Ok(false) => this.with_active_tab(|tab| {
                    tab.set_status("That folder was already removed from Places.");
                }),
                Err(error) => this.with_active_tab(|tab| {
                    tab.set_status(&format!("Could not remove bookmark: {error}"));
                }),
            }
        });
        self.window.add_action(&bookmark_remove);

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
        let this = self.clone();
        file_move_to.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.choose_destination_for_selected(ClipboardOp::Cut));
        });
        self.window.add_action(&file_move_to);

        let file_copy_to = gio::SimpleAction::new("file-copy-to", None);
        let this = self.clone();
        file_copy_to.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.choose_destination_for_selected(ClipboardOp::Copy));
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

        let file_extract_here = gio::SimpleAction::new("file-extract-here", None);
        let this = self.clone();
        file_extract_here.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.extract_selected_archive_here());
        });
        self.window.add_action(&file_extract_here);

        let file_extract_to = gio::SimpleAction::new("file-extract-to", None);
        let this = self.clone();
        file_extract_to.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.choose_archive_extraction_destination());
        });
        self.window.add_action(&file_extract_to);

        let file_move_to_trash = gio::SimpleAction::new("file-move-to-trash", None);
        let this = self.clone();
        file_move_to_trash.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.trash_selected());
        });
        self.window.add_action(&file_move_to_trash);

        let file_restore_from_trash = gio::SimpleAction::new("file-restore-from-trash", None);
        let this = self.clone();
        file_restore_from_trash.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.restore_selected_from_trash());
        });
        self.window.add_action(&file_restore_from_trash);

        let file_delete_permanently = gio::SimpleAction::new("file-delete-permanently", None);
        let this = self.clone();
        file_delete_permanently.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.show_delete_permanently_confirmation());
        });
        self.window.add_action(&file_delete_permanently);

        let file_properties = gio::SimpleAction::new("file-properties", None);
        let this = self.clone();
        file_properties.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.show_properties_dialog());
        });
        self.window.add_action(&file_properties);

        let file_undo = gio::SimpleAction::new("file-undo", None);
        let this = self.clone();
        file_undo.connect_activate(move |_, _| {
            this.with_active_tab(|tab| tab.undo_last_operation());
        });
        self.window.add_action(&file_undo);

        let this = self.clone();
        let pending_toast_undo = self.pending_toast_undo.clone();
        self.toasts.connect_action(move || {
            let Some(tab_name) = pending_toast_undo.borrow_mut().take() else {
                return;
            };
            if let Some(tab) = this
                .tabs
                .borrow()
                .iter()
                .find(|tab| tab.tab_name == tab_name)
                .cloned()
            {
                this.stack.set_visible_child_name(&tab_name);
                tab.undo_last_operation();
            }
        });
    }

    fn show_bookmark_rename_dialog(&self, uri: String) {
        let bookmark = match load_local_bookmarks() {
            Ok(bookmarks) => bookmarks.into_iter().find(|bookmark| bookmark.uri == uri),
            Err(error) => {
                self.with_active_tab(|tab| {
                    tab.set_status(&format!("Could not read Places bookmarks: {error}"));
                });
                return;
            }
        };
        let Some(bookmark) = bookmark else {
            self.with_active_tab(|tab| tab.set_status("That Places bookmark no longer exists."));
            return;
        };

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Rename Places Bookmark")
            .default_width(380)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let label = gtk::Label::new(Some("Bookmark name"));
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        let initial_name = bookmark
            .label
            .clone()
            .unwrap_or_else(|| local_bookmark_default_name(&bookmark.path));
        entry.set_text(&initial_name);
        entry.select_region(0, -1);
        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error");
        error.set_visible(false);
        let cancel = gtk::Button::with_label("Cancel");
        let rename = gtk::Button::with_label("Rename");
        rename.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&rename);
        content.append(&label);
        content.append(&entry);
        content.append(&error);
        content.append(&actions);
        dialog.set_child(Some(&content));
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_rename = dialog.clone();
        let entry_for_rename = entry.clone();
        rename.connect_clicked(move |_| {
            match rename_local_bookmark(&uri, entry_for_rename.text().as_str()) {
                Ok(true) => {
                    dialog_for_rename.close();
                    this.reload_places();
                    this.with_active_tab(|tab| tab.set_status("Renamed Places bookmark."));
                }
                Ok(false) => {
                    error.set_text("That Places bookmark no longer exists.");
                    error.set_visible(true);
                }
                Err(rename_error) => {
                    error.set_text(&rename_error.to_string());
                    error.set_visible(true);
                }
            }
        });
        dialog.present();
        entry.grab_focus();
    }

    fn show_connect_to_server_dialog(&self) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Connect to Server")
            .default_width(460)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let heading = gtk::Label::new(Some("Server address"));
        heading.set_xalign(0.0);
        heading.add_css_class("title-3");
        let help = gtk::Label::new(Some(
            "Use smb://, sftp://, ftp://, ftps://, dav://, or davs://. Credentials are requested by the system and are never saved in the bookmark.",
        ));
        help.set_xalign(0.0);
        help.set_wrap(true);
        help.add_css_class("dim-label");
        let address = gtk::Entry::new();
        address.set_placeholder_text(Some("smb://server/share"));
        address.set_input_purpose(gtk::InputPurpose::Url);
        let remember = gtk::CheckButton::with_label("Remember this server");
        remember.set_active(true);
        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error");
        error.set_visible(false);

        let cancel = gtk::Button::with_label("Cancel");
        let connect = gtk::Button::with_label("Connect");
        connect.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&connect);

        content.append(&heading);
        content.append(&help);
        content.append(&address);
        content.append(&remember);
        content.append(&error);
        content.append(&actions);
        dialog.set_child(Some(&content));
        dialog.set_default_widget(Some(&connect));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let connect_for_entry = connect.clone();
        address.connect_activate(move |_| connect_for_entry.emit_clicked());

        let this = self.clone();
        let dialog_for_connect = dialog.clone();
        let address_for_connect = address.clone();
        connect.connect_clicked(move |_| {
            let uri = match validate_network_uri(address_for_connect.text().as_str()) {
                Ok(uri) => uri,
                Err(message) => {
                    error.set_text(&message);
                    error.set_visible(true);
                    return;
                }
            };

            if remember.is_active() {
                if let Err(err) = add_network_bookmark(&uri) {
                    error.set_text(&format!("Could not save bookmark: {err}"));
                    error.set_visible(true);
                    return;
                }
                for tab in this.tabs.borrow().iter() {
                    tab.load_places();
                }
            }

            dialog_for_connect.close();
            this.with_active_tab(|tab| tab.connect_network_location(&uri));
        });

        dialog.present();
        address.grab_focus();
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
        if self.pending_toast_undo.borrow().as_deref() == Some(active_name.as_str()) {
            self.pending_toast_undo.borrow_mut().take();
            self.toasts.set_action_label(None);
        }

        let next_index = if index > 0 { index - 1 } else { 0 };
        if let Some(next_tab) = tabs.get(next_index) {
            self.stack.set_visible_child_name(&next_tab.tab_name);
        }
    }

    fn add_location_tab(&self, location: Location) {
        let tab_name = self.next_tab_name();
        let tab_name_for_stack = tab_name.clone();
        let tab = create_file_manager_page(
            &self.window,
            tab_name,
            &self.toasts,
            &self.pending_toast_undo,
        );
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
    app.set_accels_for_action("win.file-delete-permanently", &["<Shift>Delete"]);
    app.set_accels_for_action("win.file-rename", &["F2"]);
    app.set_accels_for_action("win.file-properties", &["<Alt>Return"]);
    app.set_accels_for_action("win.file-select-all", &["<Control>a"]);
    app.set_accels_for_action("win.file-undo", &["<Control>z"]);
    app.set_accels_for_action("win.bookmark-current", &["<Control>d"]);
    app.set_accels_for_action("win.tab-new", &["<Control>t"]);
    app.set_accels_for_action("win.tab-close", &["<Control>w"]);
    app.run();
}

fn build_ui(app: &adw::Application, initial_path: Option<PathBuf>) {
    install_focaldesk_theme();
    let window = adw::ApplicationWindow::new(app);
    window.add_css_class("focaldesk-app");
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
    tab_bar.add_css_class("files-tabbar");
    tab_bar.set_margin_top(8);
    tab_bar.set_margin_bottom(8);
    tab_bar.set_margin_start(12);
    tab_bar.set_margin_end(12);
    tab_bar.append(&switcher);
    tab_bar.append(&new_tab_button);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("files-root");
    outer.append(&tab_bar);
    outer.append(&stack);
    let toasts = ToastOverlay::new(&outer);
    window.set_content(Some(&toasts.widget()));

    let tabs = TabManager::new(&window, &stack, &toasts);
    tabs.install_actions();

    let tabs_for_button = tabs.clone();
    new_tab_button.connect_clicked(move |_| {
        gtk::prelude::ActionGroupExt::activate_action(&tabs_for_button.window, "tab-new", None);
    });
    let tabs_for_close = tabs.clone();
    window.connect_close_request(move |_| {
        for tab in tabs_for_close.tabs.borrow().iter() {
            for record in tab.undo_history.borrow().iter() {
                cleanup_undo_record(record);
            }
        }
        glib::Propagation::Proceed
    });

    ensure_standard_user_dirs();
    tabs.add_location_tab(Location::Path(initial_path.unwrap_or_else(home_dir)));
    window.present();
}

fn install_focaldesk_theme() {
    let provider = gtk::CssProvider::new();
    let initial = active_theme_snapshot();
    apply_theme_snapshot(&provider, &initial);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let current = Rc::new(RefCell::new(initial));
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
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

fn create_file_manager_page(
    window: &adw::ApplicationWindow,
    tab_name: String,
    toasts: &ToastOverlay,
    pending_toast_undo: &Rc<RefCell<Option<String>>>,
) -> FileManager {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    // GTK renders its CSS gradients into the client's 8-bit buffer.  Dark
    // title-bar ramps therefore arrive at the compositor already quantized
    // and remain visibly banded when expanded to HDR.  Keep this surface flat;
    // the shared border and inset highlight still separate it from the view.
    header.add_css_class("files-titlebar");
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
    let bookmark_button = icon_button("bookmark-new-symbolic", "Add Current Folder to Places");
    bookmark_button.set_action_name(Some("win.bookmark-current"));
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
    header.pack_start(&bookmark_button);
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
    let device_monitor = Rc::new(DeviceMonitorSubscription::new());
    let selection = gtk::SingleSelection::new(Some(places.clone()));
    let places_view = gtk::ListView::new(Some(selection.clone()), Some(place_factory()));
    places_view.add_css_class("navigation-sidebar");
    places_view.set_vexpand(true);

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    sidebar_box.add_css_class("places-sidebar");
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);
    sidebar_box.append(&places_view);
    let connect_server_button = gtk::Button::with_label("Connect to Server…");
    connect_server_button.set_action_name(Some("win.connect-server"));
    connect_server_button.set_tooltip_text(Some("Connect to a network file server"));
    sidebar_box.append(&connect_server_button);
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

    let recursive_search = gtk::ToggleButton::with_label("Subfolders");
    recursive_search.set_tooltip_text(Some("Search filenames in this folder and its subfolders"));

    let sort_dropdown = gtk::DropDown::from_strings(&["Name", "Size", "Type", "Modified"]);
    sort_dropdown.set_selected(0);

    let sort_descending = gtk::ToggleButton::new();
    sort_descending.set_icon_name("view-sort-descending-symbolic");
    sort_descending.set_tooltip_text(Some("Toggle sort order"));

    let status = StatusBanner::new("Ready");

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.set_margin_top(8);
    controls.append(&search_entry);
    controls.append(&recursive_search);
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
    let preview_picture = gtk::Picture::new();
    preview_picture.set_size_request(260, 220);
    preview_picture.set_can_shrink(true);
    preview_picture.set_content_fit(gtk::ContentFit::Contain);
    preview_picture.set_visible(false);
    let preview_title = gtk::Label::new(Some("Select a file"));
    preview_title.set_xalign(0.0);
    preview_title.set_wrap(true);
    preview_title.add_css_class("title-3");
    let preview_metadata = gtk::Label::new(None);
    preview_metadata.set_xalign(0.0);
    preview_metadata.set_wrap(true);
    preview_metadata.set_selectable(true);
    preview_metadata.add_css_class("dim-label");
    let preview_text = gtk::Label::new(None);
    preview_text.set_xalign(0.0);
    preview_text.set_yalign(0.0);
    preview_text.set_wrap(true);
    preview_text.set_selectable(true);
    preview_text.add_css_class("monospace");

    let preview_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    preview_box.add_css_class("file-preview");
    preview_box.set_margin_start(12);
    preview_box.append(&preview_picture);
    preview_box.append(&preview_title);
    preview_box.append(&preview_metadata);
    preview_box.append(&preview_text);
    let preview_scroll = gtk::ScrolledWindow::new();
    preview_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    preview_scroll.set_min_content_width(260);
    preview_scroll.set_child(Some(&preview_box));

    let browser = gtk::Paned::new(gtk::Orientation::Horizontal);
    browser.set_wide_handle(true);
    browser.set_resize_end_child(false);
    browser.set_shrink_end_child(true);
    browser.set_start_child(Some(&scroller));
    browser.set_end_child(Some(&preview_scroll));
    browser.set_position(680);
    browser.set_vexpand(true);
    content.append(&browser);
    content.append(&status.widget());
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
        toasts: toasts.clone(),
        hidden_toggle,
        places,
        device_monitor,
        view_mode: Rc::new(RefCell::new(ViewMode::Details)),
        sort_mode: Rc::new(RefCell::new(SortMode::Name)),
        sort_descending: Rc::new(RefCell::new(false)),
        search_query: Rc::new(RefCell::new(String::new())),
        recursive_search_enabled: Rc::new(RefCell::new(false)),
        recursive_search_result: Rc::new(RefCell::new(None)),
        search_generation: Arc::new(AtomicU64::new(0)),
        pending_search: Rc::new(RefCell::new(None)),
        current_location: Rc::new(RefCell::new(Location::Path(home_dir()))),
        entries: Rc::new(RefCell::new(Vec::new())),
        visible_entries: Rc::new(RefCell::new(Vec::new())),
        file_clipboard: Rc::new(RefCell::new(None)),
        undo_history: Rc::new(RefCell::new(VecDeque::new())),
        pending_toast_undo: pending_toast_undo.clone(),
        back_stack: Rc::new(RefCell::new(Vec::new())),
        forward_stack: Rc::new(RefCell::new(Vec::new())),
        context_menu_index: Rc::new(RefCell::new(None)),
        tab_page: Rc::new(RefCell::new(None)),
        directory_monitor: Rc::new(RefCell::new(None)),
        pending_directory_reload: Rc::new(RefCell::new(None)),
        directory_poll: Rc::new(RefCell::new(None)),
        directory_revision: Rc::new(RefCell::new(None)),
        preview_picture,
        preview_title,
        preview_metadata,
        preview_text,
        thumbnail_generation: Arc::new(AtomicU64::new(0)),
        preview_generation: Arc::new(AtomicU64::new(0)),
    };

    ensure_standard_user_dirs();
    manager.install_css();
    manager.load_places();
    manager.watch_devices();
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
    manager.connect_search_and_sort_actions(
        search_entry,
        recursive_search,
        sort_dropdown,
        sort_descending,
    );
    manager
}

impl FileManager {
    fn connect_search_and_sort_actions(
        &self,
        search_entry: gtk::SearchEntry,
        recursive_search: gtk::ToggleButton,
        sort_dropdown: gtk::DropDown,
        sort_descending: gtk::ToggleButton,
    ) {
        let this = self.clone();
        search_entry.connect_search_changed(move |entry| {
            this.set_search_query(entry.text().to_string());
        });

        let this = self.clone();
        recursive_search.connect_toggled(move |button| {
            this.set_recursive_search(button.is_active());
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
            this.activate_place(position);
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
            .file-grid-thumbnail {
                min-width: 96px;
                min-height: 72px;
            }
            .file-grid-name {
                font-weight: 500;
            }
            .file-preview {
                padding: 4px 8px 12px 12px;
            }
            .file-name {
                font-weight: 500;
            }
            .column-header {
                padding: 0 10px 4px 46px;
            }
            window.focaldesk-app headerbar.files-titlebar {
                background-image: none;
                background-color: @fd_app_surface_raised;
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
        populate_places(&self.places, &self.device_monitor.monitor);
    }

    fn choose_projects_location(&self) {
        let initial_folder = gio::File::for_path(projects_location());
        let dialog = gtk::FileDialog::builder()
            .title("Choose Projects Folder")
            .accept_label("Use as Projects")
            .modal(true)
            .initial_folder(&initial_folder)
            .build();
        let this = self.clone();
        dialog.select_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(folder) => {
                    let Some(path) = folder.path() else {
                        this.set_status("Projects must be a local folder.");
                        return;
                    };
                    if !path.is_dir() {
                        this.set_status("The selected Projects location is not a folder.");
                        return;
                    }
                    match save_projects_location(&path) {
                        Ok(()) => {
                            this.load_places();
                            this.set_status("Updated the Projects shortcut.");
                        }
                        Err(error) => this
                            .set_status(&format!("Could not save the Projects location: {error}")),
                    }
                }
                Err(error) if !error.matches(gio::IOErrorEnum::Cancelled) => {
                    this.set_status(&format!("Could not choose a Projects folder: {error}"));
                }
                Err(_) => {}
            },
        );
    }

    fn watch_devices(&self) {
        let connect_refresh = |connect: &dyn Fn(Box<dyn Fn()>) -> glib::SignalHandlerId| {
            let places = self.places.downgrade();
            let handler = connect(Box::new(move || {
                if let Some(places) = places.upgrade() {
                    populate_places(&places, &gio::VolumeMonitor::get());
                }
            }));
            self.device_monitor.handlers.borrow_mut().push(handler);
        };

        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_volume_added(move |_, _| refresh()));
        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_volume_removed(move |_, _| refresh()));
        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_volume_changed(move |_, _| refresh()));
        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_mount_added(move |_, _| refresh()));
        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_mount_removed(move |_, _| refresh()));
        let monitor = self.device_monitor.monitor.clone();
        connect_refresh(&|refresh| monitor.connect_mount_changed(move |_, _| refresh()));
    }

    fn activate_place(&self, position: u32) {
        let Some(text) = self.places.string(position) else {
            return;
        };
        let route = text.lines().nth(2).unwrap_or_default();
        if let Some(key) = route.strip_prefix("@volume:") {
            self.mount_volume_and_open(key);
            return;
        }
        if let Some(uri) = route.strip_prefix("@mount:") {
            self.open_location(location_for_file(&gio::File::for_uri(uri)), true);
            return;
        }
        if let Some(uri) = route.strip_prefix("@network:") {
            self.connect_network_location(uri);
            return;
        }
        if let Some(uri) = route.strip_prefix("@bookmark:") {
            let Some(path) = local_bookmark_path(uri) else {
                self.set_status("That Places bookmark is invalid.");
                return;
            };
            if path.is_dir() {
                self.open_location(Location::Path(path), true);
            } else {
                self.set_status("That bookmarked folder is currently unavailable.");
            }
            return;
        }

        if let Some(location) = text.lines().nth(1).map(location_from_sidebar_path) {
            self.open_location(location, true);
        }
    }

    fn mount_volume_and_open(&self, key: &str) {
        let Some(volume) = self
            .device_monitor
            .monitor
            .volumes()
            .into_iter()
            .find(|volume| volume_key(volume) == key)
        else {
            self.set_status("That device is no longer available.");
            self.load_places();
            return;
        };

        if let Some(mount) = volume.get_mount() {
            self.open_location(location_for_file(&mount.root()), true);
            return;
        }
        if !volume.can_mount() {
            self.set_status(&format!("{} cannot be mounted.", volume.name()));
            return;
        }

        let name = volume.name().to_string();
        self.set_status(&format!("Mounting {name}…"));
        let operation = gtk::MountOperation::new(Some(&self.window));
        let this = self.clone();
        let mounted_volume = volume.clone();
        volume.mount(
            gio::MountMountFlags::NONE,
            Some(&operation),
            gio::Cancellable::NONE,
            move |result| {
                this.load_places();
                match result {
                    Ok(()) => {
                        if let Some(mount) = mounted_volume.get_mount() {
                            this.set_status(&format!("Mounted {name}."));
                            this.open_location(location_for_file(&mount.root()), true);
                        } else {
                            this.set_status(&format!(
                                "{name} mounted, but its contents are not available yet."
                            ));
                        }
                    }
                    Err(err) => {
                        this.set_status(&format!("Could not unlock or mount {name}: {err}"))
                    }
                }
            },
        );
    }

    fn connect_network_location(&self, uri: &str) {
        let uri = match validate_network_uri(uri) {
            Ok(uri) => uri,
            Err(message) => {
                self.set_status(&message);
                return;
            }
        };
        let location = gio::File::for_uri(&uri);
        let operation = gtk::MountOperation::new(Some(&self.window));
        self.set_status("Connecting to server…");
        let this = self.clone();
        location.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&operation),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(()) => {
                    this.load_places();
                    this.open_location(Location::Uri(uri), true);
                }
                Err(err) if err.matches(gio::IOErrorEnum::AlreadyMounted) => {
                    this.load_places();
                    this.open_location(Location::Uri(uri), true);
                }
                Err(err) if err.matches(gio::IOErrorEnum::Cancelled) => {
                    this.set_status("Server connection cancelled.");
                }
                Err(err) => {
                    this.set_status(&network_connection_error(&err));
                }
            },
        );
    }

    fn unmount_device(&self, uri: &str, eject: bool) {
        let Some(mount) = self
            .device_monitor
            .monitor
            .mounts()
            .into_iter()
            .find(|mount| mount.root().uri() == uri)
        else {
            self.set_status("That device is no longer mounted.");
            self.load_places();
            return;
        };

        let name = mount.name().to_string();
        if eject && !mount.can_eject() {
            self.set_status(&format!("{name} cannot be ejected."));
            return;
        }
        if !eject && !mount.can_unmount() {
            self.set_status(&format!("{name} cannot be unmounted."));
            return;
        }

        self.leave_mount_if_open(&mount);
        self.set_status(&format!(
            "{} {name}…",
            if eject { "Ejecting" } else { "Unmounting" }
        ));
        let operation = gtk::MountOperation::new(Some(&self.window));
        let this = self.clone();
        let finish = move |result: Result<(), glib::Error>| {
            this.load_places();
            match result {
                Ok(()) => this.set_status(&format!(
                    "{name} {} safely.",
                    if eject { "ejected" } else { "unmounted" }
                )),
                Err(err) => this.set_status(&format!(
                    "Could not {} {name}: {err}",
                    if eject { "eject" } else { "unmount" }
                )),
            }
        };

        if eject {
            mount.eject_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&operation),
                gio::Cancellable::NONE,
                finish,
            );
        } else {
            mount.unmount_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&operation),
                gio::Cancellable::NONE,
                finish,
            );
        }
    }

    fn eject_volume(&self, key: &str) {
        let Some(volume) = self
            .device_monitor
            .monitor
            .volumes()
            .into_iter()
            .find(|volume| volume_key(volume) == key)
        else {
            self.set_status("That device is no longer available.");
            self.load_places();
            return;
        };
        if let Some(mount) = volume.get_mount() {
            self.unmount_device(mount.root().uri().as_str(), true);
            return;
        }
        if !volume.can_eject() {
            self.set_status(&format!("{} cannot be ejected.", volume.name()));
            return;
        }

        let name = volume.name().to_string();
        self.set_status(&format!("Ejecting {name}…"));
        let operation = gtk::MountOperation::new(Some(&self.window));
        let this = self.clone();
        volume.eject_with_operation(
            gio::MountUnmountFlags::NONE,
            Some(&operation),
            gio::Cancellable::NONE,
            move |result| {
                this.load_places();
                match result {
                    Ok(()) => this.set_status(&format!("{name} ejected safely.")),
                    Err(err) => this.set_status(&format!("Could not eject {name}: {err}")),
                }
            },
        );
    }

    fn leave_mount_if_open(&self, mount: &gio::Mount) {
        let root = mount.root();
        let current = match &*self.current_location.borrow() {
            Location::Path(path) => gio::File::for_path(path),
            Location::Uri(uri) => gio::File::for_uri(uri),
            Location::Trash | Location::Separator => return,
        };
        if current == root || current.has_prefix(&root) {
            self.back_stack.borrow_mut().clear();
            self.forward_stack.borrow_mut().clear();
            self.open_location(Location::Path(home_dir()), false);
        }
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
                self.restart_recursive_search();
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
        let thumbnail_generation = self.thumbnail_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.show_selection_preview(&[]);
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
        let recursive_active = self.recursive_search_active();
        let recursive_result = self.recursive_search_result.borrow().clone();
        let mut visible_entries = if recursive_active {
            recursive_result
                .as_ref()
                .map(|result| result.items.clone())
                .unwrap_or_default()
        } else {
            let mut items = self.entries.borrow().clone();
            items.retain(|item| file_matches_search(item, &search_query));
            items
        };
        sort_file_items(&mut visible_entries, sort_mode, sort_descending);
        *self.visible_entries.borrow_mut() = visible_entries.clone();
        let mut thumbnail_jobs = Vec::new();
        let is_trash = self.current_location.borrow().is_trash();

        for item in visible_entries.iter() {
            let row = file_row(item, view_mode);
            attach_file_drag_source(&row, item.clone());
            let list = self.list.clone();
            let row_for_context = row.clone();
            let context_index = self.context_menu_index.clone();
            let this = self.clone();
            let this_for_favorite = self.clone();
            let item_for_favorite = item.clone();
            attach_file_context_menu(
                &row,
                file_context_target(item),
                item.uri.clone(),
                is_trash,
                item.path
                    .as_deref()
                    .is_some_and(|path| archive_kind(path).is_some()),
                move || {
                    if !row_for_context.is_selected() {
                        list.unselect_all();
                        list.select_row(Some(&row_for_context));
                    }
                    *context_index.borrow_mut() = Some(row_for_context.index());
                },
                move || this_for_favorite.toggle_launcher_favorite(&item_for_favorite),
                move || {
                    let this = this.clone();
                    glib::idle_add_local_once(move || this.show_rename_dialog());
                },
            );
            self.list.append(&row);

            let (child, thumbnail_target) = grid_file_child(item);
            if view_mode == ViewMode::Grid {
                if let Some(request) = thumbnail_request(
                    &item.uri,
                    item.path.as_deref(),
                    &item.content_type,
                    item.size,
                ) {
                    thumbnail_jobs.push(GridThumbnailJob {
                        request,
                        stack: thumbnail_target.0,
                        picture: thumbnail_target.1,
                    });
                }
            }
            attach_file_drag_source(&child, item.clone());
            let grid = self.grid.clone();
            let child_for_context = child.clone();
            let context_index = self.context_menu_index.clone();
            let this = self.clone();
            let this_for_favorite = self.clone();
            let item_for_favorite = item.clone();
            attach_file_context_menu(
                &child,
                file_context_target(item),
                item.uri.clone(),
                is_trash,
                item.path
                    .as_deref()
                    .is_some_and(|path| archive_kind(path).is_some()),
                move || {
                    if !child_for_context.is_selected() {
                        grid.unselect_all();
                        grid.select_child(&child_for_context);
                    }
                    *context_index.borrow_mut() = Some(child_for_context.index());
                },
                move || this_for_favorite.toggle_launcher_favorite(&item_for_favorite),
                move || {
                    let this = this.clone();
                    glib::idle_add_local_once(move || this.show_rename_dialog());
                },
            );
            self.grid.append(&child);
        }
        thumbnail_jobs.truncate(64);
        self.start_thumbnail_jobs(thumbnail_generation, thumbnail_jobs);

        if recursive_active {
            let Some(result) = recursive_result else {
                self.set_status("Searching subfolders…");
                return;
            };
            let mut notes = Vec::new();
            if result.result_limit_reached {
                notes.push("result limit reached".to_string());
            }
            if result.directory_limit_reached {
                notes.push("folder limit reached".to_string());
            }
            if result.depth_limit_reached {
                notes.push("depth limit reached".to_string());
            }
            if result.skipped_dirs > 0 {
                notes.push(format!(
                    "skipped {} inaccessible folder{}",
                    result.skipped_dirs,
                    plural(result.skipped_dirs)
                ));
            }
            let suffix = if notes.is_empty() {
                String::new()
            } else {
                format!(" — partial: {}", notes.join(", "))
            };
            self.set_status(&format!(
                "Found {} result{} in {} folder{}{}",
                visible_entries.len(),
                plural(visible_entries.len()),
                result.scanned_dirs,
                plural(result.scanned_dirs),
                suffix
            ));
            return;
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
                    Ok(()) => {
                        this.reload();
                        this.remember_undo(
                            "new folder",
                            vec![UndoStep::RemoveCreated {
                                path,
                                empty_only: true,
                            }],
                            "Created folder.",
                        );
                    }
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
        self.context_menu_index.borrow_mut().take();
        let items = self.selected_file_items();
        if items.is_empty() {
            self.set_status("Select one or more items to rename.");
            return;
        }
        if items.len() > 1 {
            self.show_bulk_rename_dialog(items);
            return;
        }

        self.show_rename_dialog_for_item(items[0].clone());
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

            match move_no_replace(&source_path, &destination) {
                Ok(()) => {
                    this.reload();
                    this.remember_undo(
                        "rename",
                        vec![UndoStep::MoveBack {
                            from: destination.clone(),
                            to: gio::File::for_path(&source_path),
                        }],
                        &format!("Renamed {} to {}.", item.name, name),
                    );
                    dialog_for_rename.close();
                }
                Err(err) => {
                    this.set_status(&format!("Could not rename {}: {err}", item.name));
                }
            }
        });

        dialog.present();
    }

    fn show_bulk_rename_dialog(&self, items: Vec<FileItem>) {
        if items.len() > MAX_BULK_RENAME_ITEMS {
            self.set_status(&format!(
                "Bulk rename is limited to {MAX_BULK_RENAME_ITEMS} items at a time."
            ));
            return;
        }
        let mut paths = Vec::with_capacity(items.len());
        for item in &items {
            let Some(path) = item.path.clone() else {
                self.set_status("Only local files and folders can be renamed.");
                return;
            };
            paths.push(path);
        }

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Rename Multiple Items")
            .default_width(700)
            .default_height(560)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let mode = gtk::DropDown::from_strings(&[
            "Find and Replace",
            "Add Prefix",
            "Add Suffix",
            "Sequential Numbering",
            "Lowercase",
            "Uppercase",
        ]);
        let primary_label = gtk::Label::new(Some("Find"));
        primary_label.set_xalign(0.0);
        let primary = gtk::Entry::new();
        let secondary_label = gtk::Label::new(Some("Replace with"));
        secondary_label.set_xalign(0.0);
        let secondary = gtk::Entry::new();
        let preserve_extension = gtk::CheckButton::with_label("Preserve file extensions");
        preserve_extension.set_active(true);

        let controls = gtk::Grid::new();
        controls.set_column_spacing(10);
        controls.set_row_spacing(8);
        controls.attach(&gtk::Label::new(Some("Rule")), 0, 0, 1, 1);
        controls.attach(&mode, 1, 0, 1, 1);
        controls.attach(&primary_label, 0, 1, 1, 1);
        controls.attach(&primary, 1, 1, 1, 1);
        controls.attach(&secondary_label, 0, 2, 1, 1);
        controls.attach(&secondary, 1, 2, 1, 1);
        controls.attach(&preserve_extension, 1, 3, 1, 1);
        primary.set_hexpand(true);
        secondary.set_hexpand(true);
        content.append(&controls);

        let preview_heading = gtk::Label::new(Some("Preview"));
        preview_heading.set_xalign(0.0);
        preview_heading.add_css_class("heading");
        content.append(&preview_heading);
        let preview = gtk::ListBox::new();
        preview.set_selection_mode(gtk::SelectionMode::None);
        preview.add_css_class("boxed-list");
        let preview_scroll = gtk::ScrolledWindow::new();
        preview_scroll.set_vexpand(true);
        preview_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        preview_scroll.set_child(Some(&preview));
        content.append(&preview_scroll);

        let validation = gtk::Label::new(None);
        validation.set_xalign(0.0);
        validation.set_wrap(true);
        validation.add_css_class("dim-label");
        content.append(&validation);
        let cancel = gtk::Button::with_label("Cancel");
        let rename = gtk::Button::with_label("Rename");
        rename.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&rename);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let paths_for_preview = paths.clone();
        let refresh: Rc<dyn Fn()> = Rc::new({
            let mode = mode.clone();
            let primary = primary.clone();
            let secondary = secondary.clone();
            let primary_label = primary_label.clone();
            let secondary_label = secondary_label.clone();
            let preserve_extension = preserve_extension.clone();
            let preview = preview.clone();
            let validation = validation.clone();
            let rename = rename.clone();
            move || {
                let rename_mode = bulk_rename_mode(mode.selected());
                configure_bulk_rename_fields(
                    rename_mode,
                    &primary_label,
                    &primary,
                    &secondary_label,
                    &secondary,
                );
                while let Some(row) = preview.first_child() {
                    preview.remove(&row);
                }
                let options = BulkRenameOptions {
                    mode: rename_mode,
                    primary: primary.text().to_string(),
                    secondary: secondary.text().to_string(),
                    preserve_extension: preserve_extension.is_active(),
                };
                match build_bulk_rename_plan(&paths_for_preview, &options) {
                    Ok(plan) => {
                        for (old_name, new_name) in &plan.mappings {
                            preview.append(&bulk_rename_preview_row(old_name, new_name));
                        }
                        validation.set_text(&format!(
                            "{} item{} will be renamed.",
                            plan.moves.len(),
                            plural(plan.moves.len())
                        ));
                        validation.remove_css_class("error");
                        rename.set_sensitive(!plan.moves.is_empty());
                    }
                    Err(error) => {
                        validation.set_text(&error);
                        validation.add_css_class("error");
                        rename.set_sensitive(false);
                    }
                }
            }
        });
        let refresh_for_mode = refresh.clone();
        mode.connect_selected_notify(move |_| refresh_for_mode());
        let refresh_for_primary = refresh.clone();
        primary.connect_changed(move |_| refresh_for_primary());
        let refresh_for_secondary = refresh.clone();
        secondary.connect_changed(move |_| refresh_for_secondary());
        let refresh_for_extension = refresh.clone();
        preserve_extension.connect_toggled(move |_| refresh_for_extension());

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_rename = dialog.clone();
        rename.connect_clicked(move |_| {
            let options = BulkRenameOptions {
                mode: bulk_rename_mode(mode.selected()),
                primary: primary.text().to_string(),
                secondary: secondary.text().to_string(),
                preserve_extension: preserve_extension.is_active(),
            };
            match build_bulk_rename_plan(&paths, &options) {
                Ok(plan) if !plan.moves.is_empty() => {
                    dialog_for_rename.close();
                    this.start_bulk_rename(plan);
                }
                Ok(_) => validation.set_text("No names would change."),
                Err(error) => validation.set_text(&error),
            }
        });
        refresh();
        dialog.present();
    }

    fn start_bulk_rename(&self, plan: BulkRenamePlan) {
        let count = plan.moves.len();
        let moves = plan.moves;
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Renaming Items")
            .default_width(420)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let label = gtk::Label::new(Some(&format!(
            "Renaming {count} item{} safely…",
            plural(count)
        )));
        label.set_xalign(0.0);
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_text(Some("Checking names and applying transaction"));
        content.append(&label);
        content.append(&progress);
        dialog.set_child(Some(&content));
        dialog.present();

        let worker_moves = moves.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = execute_rename_transaction(&worker_moves);
            let _ = sender.send(result);
        });
        let this = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            progress.pulse();
            match receiver.try_recv() {
                Ok(Ok(())) => {
                    dialog.close();
                    this.reload();
                    let undo_moves = moves
                        .iter()
                        .map(|(source, destination)| (destination.clone(), source.clone()))
                        .collect();
                    this.remember_undo(
                        "bulk rename",
                        vec![UndoStep::BulkRename { moves: undo_moves }],
                        &format!("Renamed {count} item{}.", plural(count)),
                    );
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    dialog.close();
                    this.reload();
                    this.set_status(&format!("Could not complete bulk rename: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    dialog.close();
                    this.reload();
                    this.set_status("Bulk rename task ended unexpectedly.");
                    ControlFlow::Break
                }
            }
        });
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

    fn selected_archive_path(&self) -> Option<PathBuf> {
        let items = self.selected_file_items();
        if items.len() != 1 {
            self.set_status("Select one local archive to extract.");
            return None;
        }
        let Some(path) = items[0].path.clone() else {
            self.set_status("Only local archives can be extracted.");
            return None;
        };
        if archive_kind(&path).is_none() {
            self.set_status("Select a .zip, .tar, .tar.gz, or .tar.xz archive.");
            return None;
        }
        Some(path)
    }

    fn extract_selected_archive_here(&self) {
        let Some(source) = self.selected_archive_path() else {
            return;
        };
        let Some(parent) = source.parent().map(Path::to_path_buf) else {
            self.set_status("The archive has no destination folder.");
            return;
        };
        self.prepare_archive_extraction(source, parent);
    }

    fn choose_archive_extraction_destination(&self) {
        let Some(source) = self.selected_archive_path() else {
            return;
        };
        let initial_folder = source
            .parent()
            .map(gio::File::for_path)
            .unwrap_or_else(|| gio::File::for_path(home_dir()));
        let dialog = gtk::FileDialog::builder()
            .title("Extract To")
            .accept_label("Extract Here")
            .modal(true)
            .initial_folder(&initial_folder)
            .build();
        let this = self.clone();
        dialog.select_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(folder) => {
                    let Some(parent) = folder.path() else {
                        this.set_status("Archives can only be extracted to a local folder.");
                        return;
                    };
                    this.prepare_archive_extraction(source.clone(), parent);
                }
                Err(error) if !error.matches(gio::IOErrorEnum::Cancelled) => {
                    this.set_status(&format!("Could not choose an extraction folder: {error}"));
                }
                Err(_) => {}
            },
        );
    }

    fn prepare_archive_extraction(&self, source: PathBuf, parent: PathBuf) {
        let Some(folder_name) = default_extract_name(&source) else {
            self.set_status("Could not determine an extraction folder name.");
            return;
        };
        let destination = parent.join(&folder_name);
        if !destination.exists() {
            self.start_archive_extraction(source, destination);
            return;
        }

        let alternate = available_destination(&parent, Path::new(&folder_name));
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Extraction Folder Exists")
            .default_width(440)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let heading = gtk::Label::new(Some(&format!(
            "A folder named “{}” already exists.",
            folder_name.to_string_lossy()
        )));
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.add_css_class("title-3");
        let explanation = gtk::Label::new(Some(&format!(
            "Keep Both will extract into “{}”. Existing files are never overwritten.",
            alternate.file_name().unwrap_or_default().to_string_lossy()
        )));
        explanation.set_xalign(0.0);
        explanation.set_wrap(true);
        explanation.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        let keep_both = gtk::Button::with_label("Keep Both");
        keep_both.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&keep_both);
        content.append(&heading);
        content.append(&explanation);
        content.append(&actions);
        dialog.set_child(Some(&content));
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_keep = dialog.clone();
        keep_both.connect_clicked(move |_| {
            dialog_for_keep.close();
            this.start_archive_extraction(source.clone(), alternate.clone());
        });
        dialog.present();
    }

    fn start_archive_extraction(&self, source: PathBuf, destination: PathBuf) {
        let archive_name = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let destination_name = destination
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Extracting Archive")
            .default_width(420)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let label = gtk::Label::new(Some(&format!("Extracting {archive_name}…")));
        label.set_xalign(0.0);
        label.set_wrap(true);
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_text(Some("Validating paths and sizes"));
        let cancel = gtk::Button::with_label("Cancel");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        content.append(&label);
        content.append(&progress);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_button = cancelled.clone();
        cancel.connect_clicked(move |button| {
            cancelled_for_button.store(true, Ordering::Release);
            button.set_sensitive(false);
            button.set_label("Cancelling…");
        });
        let cancelled_for_close = cancelled.clone();
        dialog.connect_close_request(move |_| {
            cancelled_for_close.store(true, Ordering::Release);
            glib::Propagation::Proceed
        });

        let (sender, receiver) = mpsc::channel::<io::Result<ExtractionReport>>();
        thread::spawn(move || {
            let result = extract_archive(&source, &destination, cancelled);
            let _ = sender.send(result);
        });

        let this = self.clone();
        let dialog_for_finish = dialog.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(75), move || {
            progress.pulse();
            match receiver.try_recv() {
                Ok(Ok(report)) => {
                    dialog_for_finish.close();
                    this.reload();
                    let message = format!(
                        "Extracted {} item{} ({}) into {destination_name}.",
                        report.entries,
                        plural(report.entries),
                        format_size(report.bytes)
                    );
                    this.remember_undo(
                        "archive extraction",
                        vec![UndoStep::RemoveExtracted {
                            paths: report.created_paths,
                        }],
                        &message,
                    );
                    ControlFlow::Break
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {
                    dialog_for_finish.close();
                    this.reload();
                    this.set_status("Archive extraction cancelled; partial output was removed.");
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    dialog_for_finish.close();
                    this.reload();
                    this.set_status(&format!(
                        "Could not extract {archive_name}: {error}. Partial output was removed."
                    ));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    dialog_for_finish.close();
                    this.reload();
                    this.set_status("Extraction task ended unexpectedly.");
                    ControlFlow::Break
                }
            }
        });
        dialog.present();
    }

    fn choose_destination_for_selected(&self, op: ClipboardOp) {
        let items = self.selected_file_items();
        if items.is_empty() {
            let verb = if matches!(op, ClipboardOp::Copy) {
                "copy"
            } else {
                "move"
            };
            self.set_status(&format!("Select one or more items to {verb}."));
            return;
        }

        let files = items.into_iter().map(|item| item.file).collect::<Vec<_>>();
        let (title, accept_label) = match op {
            ClipboardOp::Copy => ("Choose Copy Destination", "Copy Here"),
            ClipboardOp::Cut => ("Choose Move Destination", "Move Here"),
        };
        let initial_folder = match &*self.current_location.borrow() {
            Location::Path(path) => gio::File::for_path(path),
            Location::Trash | Location::Uri(_) | Location::Separator => {
                gio::File::for_path(home_dir())
            }
        };
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .accept_label(accept_label)
            .modal(true)
            .initial_folder(&initial_folder)
            .build();

        let this = self.clone();
        dialog.select_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(folder) => {
                    let Some(target_dir) = folder.path() else {
                        this.set_status("The selected destination is not a local folder.");
                        return;
                    };
                    if !target_dir.is_dir() {
                        this.set_status("The selected destination is not a folder.");
                        return;
                    }
                    if matches!(op, ClipboardOp::Cut)
                        && files.iter().all(|file| {
                            file.path()
                                .and_then(|path| path.parent().map(Path::to_path_buf))
                                .is_some_and(|parent| parent == target_dir)
                        })
                    {
                        this.set_status("The selected items are already in that folder.");
                        return;
                    }
                    this.copy_or_move_files(files, op, &target_dir);
                }
                Err(err) => {
                    if !err.matches(gio::IOErrorEnum::Cancelled) {
                        this.set_status(&format!("Could not choose a destination: {err}"));
                    }
                }
            },
        );
    }

    fn show_empty_trash_confirmation(&self) {
        let items = match read_location_items(&Location::Trash, true) {
            Ok(items) => items,
            Err(err) => {
                self.set_status(&format!("Could not read Trash: {err}"));
                return;
            }
        };
        if items.is_empty() {
            self.set_status("Trash is already empty.");
            return;
        }

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Empty Trash?")
            .default_width(420)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let heading = gtk::Label::new(Some("Permanently delete everything in Trash?"));
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.add_css_class("title-3");
        let warning = gtk::Label::new(Some(&format!(
            "This will permanently delete {} item{} and cannot be undone.",
            items.len(),
            plural(items.len())
        )));
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        let empty_button = gtk::Button::with_label("Empty Trash");
        empty_button.add_css_class("destructive-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&empty_button);
        content.append(&heading);
        content.append(&warning);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_empty = dialog.clone();
        empty_button.connect_clicked(move |_| {
            dialog_for_empty.close();
            let files = items
                .iter()
                .map(|item| (item.name.clone(), item.file.clone()))
                .collect::<Vec<_>>();
            this.run_progress_job(
                "Emptying Trash",
                files.len(),
                move |index| match files.get(index) {
                    Some((name, file)) => file
                        .delete(gio::Cancellable::NONE)
                        .map_err(|err| format!("Could not permanently delete {name}: {err}")),
                    None => Ok(()),
                },
                {
                    let this = this.clone();
                    move |outcome, completed, error| match outcome {
                        FileJobOutcome::Finished => {
                            if this.current_location.borrow().is_trash() {
                                this.reload();
                            }
                            match (completed, error) {
                                (0, Some(err)) => this.set_status(&err),
                                (count, Some(err)) => this.set_status(&format!(
                                    "Deleted {count} item{}; some items failed: {err}",
                                    plural(count)
                                )),
                                (count, None) => this.set_status(&format!(
                                    "Permanently deleted {count} item{}.",
                                    plural(count)
                                )),
                            }
                        }
                        FileJobOutcome::Cancelled => this.set_status(&format!(
                            "Cancelled after deleting {completed} item{}.",
                            plural(completed)
                        )),
                    }
                },
            );
        });
        dialog.present();
    }

    fn restore_selected_from_trash(&self) {
        if !self.current_location.borrow().is_trash() {
            self.set_status("Restore is only available in Trash.");
            return;
        }
        let items = self.selected_file_items();
        if items.is_empty() {
            self.set_status("Select one or more items to restore.");
            return;
        }

        let restored = Rc::new(RefCell::new(0_usize));
        let restored_for_job = restored.clone();
        self.run_progress_job(
            "Restoring from Trash",
            items.len(),
            move |index| {
                let Some(item) = items.get(index) else {
                    return Ok(());
                };
                let Some(original) = item.trash_original_path.as_deref() else {
                    return Err(format!(
                        "Could not restore {}: its original location is unavailable",
                        item.name
                    ));
                };
                restore_trash_item(&item.file, original)
                    .map_err(|err| format!("Could not restore {}: {err}", item.name))?;
                *restored_for_job.borrow_mut() += 1;
                Ok(())
            },
            {
                let this = self.clone();
                move |outcome, _, error| {
                    this.reload();
                    let count = *restored.borrow();
                    match (outcome, error) {
                        (FileJobOutcome::Finished, None) => this.set_status(&format!(
                            "Restored {count} item{} to the original location{}.",
                            plural(count),
                            plural(count)
                        )),
                        (FileJobOutcome::Finished, Some(err)) => this.set_status(&format!(
                            "Restored {count} item{}; some items could not be restored: {err}",
                            plural(count)
                        )),
                        (FileJobOutcome::Cancelled, _) => this.set_status(&format!(
                            "Cancelled after restoring {count} item{}.",
                            plural(count)
                        )),
                    }
                }
            },
        );
    }

    fn show_delete_permanently_confirmation(&self) {
        if !self.current_location.borrow().is_trash() {
            self.set_status("Permanent deletion is only available in Trash.");
            return;
        }
        let items = self.selected_file_items();
        if items.is_empty() {
            self.set_status("Select one or more items to delete permanently.");
            return;
        }

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Delete Permanently?")
            .default_width(420)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let heading = gtk::Label::new(Some(&format!(
            "Permanently delete {} selected item{}?",
            items.len(),
            plural(items.len())
        )));
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.add_css_class("title-3");
        let warning = gtk::Label::new(Some(
            "This removes the selected items from Trash and cannot be undone.",
        ));
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        let delete = gtk::Button::with_label("Delete Permanently");
        delete.add_css_class("destructive-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&delete);
        content.append(&heading);
        content.append(&warning);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_delete = dialog.clone();
        delete.connect_clicked(move |_| {
            dialog_for_delete.close();
            let items_for_job = items.clone();
            let deleted = Rc::new(RefCell::new(0_usize));
            let deleted_for_job = deleted.clone();
            this.run_progress_job(
                "Deleting Permanently",
                items_for_job.len(),
                move |index| {
                    let Some(item) = items_for_job.get(index) else {
                        return Ok(());
                    };
                    item.file.delete(gio::Cancellable::NONE).map_err(|err| {
                        format!("Could not permanently delete {}: {err}", item.name)
                    })?;
                    *deleted_for_job.borrow_mut() += 1;
                    Ok(())
                },
                {
                    let this = this.clone();
                    move |outcome, _, error| {
                        this.reload();
                        let count = *deleted.borrow();
                        match (outcome, error) {
                            (FileJobOutcome::Finished, None) => this.set_status(&format!(
                                "Permanently deleted {count} item{}.",
                                plural(count)
                            )),
                            (FileJobOutcome::Finished, Some(err)) => this.set_status(&format!(
                                "Deleted {count} item{}; some items failed: {err}",
                                plural(count)
                            )),
                            (FileJobOutcome::Cancelled, _) => this.set_status(&format!(
                                "Cancelled after deleting {count} item{}.",
                                plural(count)
                            )),
                        }
                    }
                },
            );
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
        let undo_steps = Rc::new(RefCell::new(Vec::new()));
        let undo_steps_for_job = undo_steps.clone();
        self.run_progress_job(
            "Moving to Trash",
            items.len(),
            move |index| match items.get(index) {
                Some(item) => match item.file.trash(gio::Cancellable::NONE) {
                    Ok(()) => {
                        if let Some(original) = item.path.clone() {
                            undo_steps_for_job
                                .borrow_mut()
                                .push(UndoStep::RestoreTrash { original });
                        }
                        Ok(())
                    }
                    Err(err) => Err(format!("Could not move {} to trash: {err}", item.name)),
                },
                None => Ok(()),
            },
            {
                let this = self.clone();
                move |outcome, completed, error| match outcome {
                    FileJobOutcome::Finished => {
                        this.reload();
                        let message = match (completed, error) {
                            (0, Some(err)) => err,
                            (count, Some(err)) => format!(
                                "Moved {count} item{} to trash; some items failed: {err}",
                                plural(count)
                            ),
                            (count, None) => {
                                format!("Moved {count} item{} to trash.", plural(count))
                            }
                        };
                        this.remember_undo("move to Trash", undo_steps.borrow().clone(), &message);
                    }
                    FileJobOutcome::Cancelled => this.remember_undo(
                        "move to Trash",
                        undo_steps.borrow().clone(),
                        &format!(
                            "Cancelled after moving {completed} item{} to trash.",
                            plural(completed)
                        ),
                    ),
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
                self.copy_or_move_files(clipboard.files, clipboard.op, &target_dir);
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
                    this.copy_or_move_files(file_list.files(), ClipboardOp::Copy, &target_dir);
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
                            Some(clipboard) => {
                                this.copy_or_move_files(clipboard.files, clipboard.op, &target_dir)
                            }
                            None => this.set_status("Clipboard does not contain files."),
                        }
                    });
                }
                Err(err) => this.set_status(&format!("Could not read clipboard: {err}")),
            },
        );
    }

    fn copy_or_move_files(&self, files: Vec<gio::File>, op: ClipboardOp, target_dir: &Path) {
        let local_sources = files
            .iter()
            .map(gio::File::path)
            .collect::<Option<Vec<_>>>();
        if let Some(sources) = local_sources {
            self.prepare_local_transfer(sources, op, target_dir.to_path_buf());
        } else {
            self.copy_or_move_files_legacy(files, op, target_dir);
        }
    }

    fn prepare_local_transfer(&self, sources: Vec<PathBuf>, op: ClipboardOp, target_dir: PathBuf) {
        if sources.is_empty() {
            self.set_status("Clipboard does not contain files.");
            return;
        }
        let conflict_count = sources
            .iter()
            .filter_map(|source| source.file_name())
            .filter(|name| path_lexists(&target_dir.join(name)))
            .count();
        if conflict_count == 0 {
            self.start_local_transfer(sources, op, target_dir, ConflictPolicy::KeepBoth);
            return;
        }

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("File Name Conflict")
            .default_width(500)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let heading = gtk::Label::new(Some(&format!(
            "{} destination item{} already exist{}.",
            conflict_count,
            plural(conflict_count),
            if conflict_count == 1 { "s" } else { "" }
        )));
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.add_css_class("title-3");
        let explanation = gtk::Label::new(Some(
            "Choose how to handle every name conflict in this transfer. Replace keeps a private backup while Undo is available.",
        ));
        explanation.set_xalign(0.0);
        explanation.set_wrap(true);
        explanation.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        let skip = gtk::Button::with_label("Skip All");
        let keep_both = gtk::Button::with_label("Keep Both");
        keep_both.add_css_class("suggested-action");
        let replace = gtk::Button::with_label("Replace All");
        replace.add_css_class("destructive-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&skip);
        actions.append(&keep_both);
        actions.append(&replace);
        content.append(&heading);
        content.append(&explanation);
        content.append(&actions);
        dialog.set_child(Some(&content));
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        for (button, policy) in [
            (skip, ConflictPolicy::Skip),
            (keep_both, ConflictPolicy::KeepBoth),
            (replace, ConflictPolicy::Replace),
        ] {
            let this = self.clone();
            let dialog = dialog.clone();
            let sources = sources.clone();
            let target_dir = target_dir.clone();
            button.connect_clicked(move |_| {
                dialog.close();
                this.start_local_transfer(sources.clone(), op, target_dir.clone(), policy);
            });
        }
        dialog.present();
    }

    fn start_local_transfer(
        &self,
        sources: Vec<PathBuf>,
        op: ClipboardOp,
        target_dir: PathBuf,
        conflict_policy: ConflictPolicy,
    ) {
        let operation = match op {
            ClipboardOp::Copy => TransferOperation::Copy,
            ClipboardOp::Cut => TransferOperation::Move,
        };
        let title = match op {
            ClipboardOp::Copy => "Copying Files",
            ClipboardOp::Cut => "Moving Files",
        };
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(false)
            .title(title)
            .default_width(500)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let current = gtk::Label::new(Some("Scanning files…"));
        current.set_xalign(0.0);
        current.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.pulse();
        progress.set_text(Some("Preparing transfer"));
        let cancel = gtk::Button::with_label("Cancel");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        content.append(&current);
        content.append(&progress);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_button = cancelled.clone();
        cancel.connect_clicked(move |button| {
            cancelled_for_button.store(true, Ordering::Release);
            button.set_sensitive(false);
            button.set_label("Cancelling…");
        });
        let cancelled_for_close = cancelled.clone();
        dialog.connect_close_request(move |_| {
            cancelled_for_close.store(true, Ordering::Release);
            glib::Propagation::Proceed
        });

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            run_transfer(
                TransferRequest {
                    sources,
                    target_dir,
                    operation,
                    conflict_policy,
                },
                cancelled,
                sender,
            );
        });

        let this = self.clone();
        let dialog_for_finish = dialog.clone();
        let mut total_bytes = None::<u64>;
        let mut started = None::<std::time::Instant>;
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let mut saw_event = false;
            loop {
                let event = match receiver.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        dialog_for_finish.close();
                        this.reload();
                        this.set_status("File transfer ended unexpectedly.");
                        return ControlFlow::Break;
                    }
                };
                saw_event = true;
                match event {
                    TransferEvent::Prepared {
                        total_bytes: bytes,
                        total_items,
                    } => {
                        total_bytes = Some(bytes);
                        started = Some(std::time::Instant::now());
                        progress.set_fraction(0.0);
                        progress.set_text(Some(&format!(
                            "0 of {} across {total_items} item{}",
                            format_size(bytes),
                            plural(total_items)
                        )));
                    }
                    TransferEvent::Progress {
                        completed_bytes,
                        current: path,
                    } => {
                        current.set_text(&path);
                        if let Some(total) = total_bytes {
                            if total > 0 {
                                progress.set_fraction(
                                    (completed_bytes as f64 / total as f64).clamp(0.0, 1.0),
                                );
                            } else {
                                progress.pulse();
                            }
                            let elapsed = started
                                .map(|started| started.elapsed().as_secs_f64())
                                .unwrap_or_default();
                            let speed = if elapsed > 0.0 {
                                completed_bytes as f64 / elapsed
                            } else {
                                0.0
                            };
                            let eta = if speed > 0.0 && completed_bytes < total {
                                format!(
                                    " — about {} remaining",
                                    format_transfer_duration(
                                        (total - completed_bytes) as f64 / speed
                                    )
                                )
                            } else {
                                String::new()
                            };
                            progress.set_text(Some(&format!(
                                "{} of {} — {}/s{eta}",
                                format_size(completed_bytes),
                                format_size(total),
                                format_size(speed.max(0.0) as u64)
                            )));
                        }
                    }
                    TransferEvent::Finished(Ok(report)) => {
                        dialog_for_finish.close();
                        this.reload();
                        if matches!(op, ClipboardOp::Cut)
                            && !report.cancelled
                            && report.errors.is_empty()
                        {
                            this.file_clipboard.borrow_mut().take();
                        }
                        let undo = report
                            .undo
                            .into_iter()
                            .map(transfer_undo_step)
                            .collect::<Vec<_>>();
                        let verb = if matches!(op, ClipboardOp::Copy) {
                            "Copied"
                        } else {
                            "Moved"
                        };
                        let mut message = if report.cancelled {
                            format!(
                                "Cancelled after {} item{}.",
                                report.completed,
                                plural(report.completed)
                            )
                        } else {
                            format!(
                                "{verb} {} item{} ({}).",
                                report.completed,
                                plural(report.completed),
                                format_size(report.bytes)
                            )
                        };
                        if report.skipped > 0 {
                            message.push_str(&format!(
                                " Skipped {} conflict{}.",
                                report.skipped,
                                plural(report.skipped)
                            ));
                        }
                        if !report.errors.is_empty() {
                            message.push_str(&format!(
                                " {} item{} failed: {}",
                                report.errors.len(),
                                plural(report.errors.len()),
                                report.errors.join("; ")
                            ));
                        }
                        this.remember_undo(
                            if matches!(op, ClipboardOp::Copy) {
                                "copy"
                            } else {
                                "move"
                            },
                            undo,
                            &message,
                        );
                        return ControlFlow::Break;
                    }
                    TransferEvent::Finished(Err(error)) => {
                        dialog_for_finish.close();
                        this.reload();
                        if error.kind() == io::ErrorKind::Interrupted {
                            this.set_status(
                                "File transfer cancelled before any changes were made.",
                            );
                        } else {
                            this.set_status(&format!("Could not start file transfer: {error}"));
                        }
                        return ControlFlow::Break;
                    }
                }
            }
            if !saw_event && total_bytes.is_none() {
                progress.pulse();
            }
            ControlFlow::Continue
        });
        dialog.present();
    }

    fn copy_or_move_files_legacy(&self, files: Vec<gio::File>, op: ClipboardOp, target_dir: &Path) {
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
        let done_verb_lower = match op {
            ClipboardOp::Copy => "copy",
            ClipboardOp::Cut => "move",
        };
        let undo_steps = Rc::new(RefCell::new(Vec::new()));
        let undo_steps_for_job = undo_steps.clone();
        self.run_progress_job(
            verb,
            files.len(),
            move |index| match files.get(index) {
                Some(file) => match op {
                    ClipboardOp::Copy => copy_dropped_file(file, &target_dir)
                        .map(|path| {
                            undo_steps_for_job
                                .borrow_mut()
                                .push(UndoStep::RemoveCreated {
                                    path,
                                    empty_only: false,
                                });
                        })
                        .map_err(|err| err.to_string()),
                    ClipboardOp::Cut => {
                        let original = file.clone();
                        move_clipboard_file(file, &target_dir)
                            .map(|destination| {
                                undo_steps_for_job.borrow_mut().push(UndoStep::MoveBack {
                                    from: destination,
                                    to: original,
                                });
                            })
                            .map_err(|err| err.to_string())
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

                        let message = match (completed, error) {
                            (0, Some(err)) => format!("Could not {done_verb_lower} files: {err}"),
                            (count, Some(err)) => format!(
                                "{done_verb} {count} item{}; some items failed: {err}",
                                plural(count)
                            ),
                            (count, None) => {
                                format!("{done_verb} {count} item{}.", plural(count))
                            }
                        };
                        this.remember_undo(done_verb_lower, undo_steps.borrow().clone(), &message);
                    }
                    FileJobOutcome::Cancelled => this.remember_undo(
                        done_verb_lower,
                        undo_steps.borrow().clone(),
                        &format!(
                            "Cancelled after {done_verb} {completed} item{}.",
                            plural(completed)
                        ),
                    ),
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

    fn remember_undo(
        &self,
        description: impl Into<String>,
        mut steps: Vec<UndoStep>,
        message: &str,
    ) {
        if steps.is_empty() {
            self.set_status(message);
            return;
        }

        steps.reverse();
        let mut history = self.undo_history.borrow_mut();
        if history.len() >= MAX_UNDO_RECORDS {
            if let Some(expired) = history.pop_front() {
                cleanup_undo_record(&expired);
            }
        }
        history.push_back(UndoRecord {
            description: description.into(),
            steps,
        });
        drop(history);

        self.status.set_text(message);
        *self.pending_toast_undo.borrow_mut() = Some(self.tab_name.clone());
        self.toasts.set_action_label(Some("Undo"));
        self.toasts.show(classify_status(message), message);
    }

    fn undo_last_operation(&self) {
        let Some(record) = self.undo_history.borrow_mut().pop_back() else {
            self.set_status("Nothing to undo.");
            return;
        };

        self.pending_toast_undo.borrow_mut().take();
        self.toasts.set_action_label(None);
        let description = record.description;
        let all_steps = record.steps;
        let steps = all_steps.clone();
        let failed_steps = Rc::new(RefCell::new(Vec::new()));
        let failed_steps_for_job = failed_steps.clone();
        self.run_progress_job(
            &format!("Undoing {description}"),
            steps.len(),
            move |index| {
                let Some(step) = steps.get(index) else {
                    return Ok(());
                };
                execute_undo_step(step).map_err(|err| {
                    failed_steps_for_job.borrow_mut().push(step.clone());
                    err.to_string()
                })
            },
            {
                let this = self.clone();
                move |outcome, completed, error| {
                    this.reload();
                    let mut failed = failed_steps.borrow().clone();
                    if matches!(outcome, FileJobOutcome::Cancelled) {
                        failed.extend(all_steps.iter().skip(completed).cloned());
                    }
                    if !failed.is_empty() {
                        let failed_count = failed.len();
                        let mut history = this.undo_history.borrow_mut();
                        if history.len() >= MAX_UNDO_RECORDS {
                            if let Some(expired) = history.pop_front() {
                                cleanup_undo_record(&expired);
                            }
                        }
                        history.push_back(UndoRecord {
                            description: description.clone(),
                            steps: failed,
                        });
                        drop(history);
                        let (kind, message) = if matches!(outcome, FileJobOutcome::Cancelled) {
                            (
                                StateKind::Info,
                                format!(
                                    "Undo cancelled; {failed_count} step{} remain.",
                                    plural(failed_count)
                                ),
                            )
                        } else {
                            (
                                StateKind::Error,
                                format!(
                                    "Could not finish undoing {description}: {} step{} failed{}",
                                    failed_count,
                                    plural(failed_count),
                                    error
                                        .as_deref()
                                        .map(|error| format!(" ({error})"))
                                        .unwrap_or_default()
                                ),
                            )
                        };
                        this.status.set_text(&message);
                        *this.pending_toast_undo.borrow_mut() = Some(this.tab_name.clone());
                        this.toasts.set_action_label(Some("Undo"));
                        this.toasts.show(kind, &message);
                        return;
                    }

                    match outcome {
                        FileJobOutcome::Finished => {
                            let message = format!("Undid {description}.");
                            this.status.set_text(&message);
                            if this.undo_history.borrow().is_empty() {
                                this.pending_toast_undo.borrow_mut().take();
                                this.toasts.set_action_label(None);
                                this.toasts.show(StateKind::Success, &message);
                            } else {
                                *this.pending_toast_undo.borrow_mut() = Some(this.tab_name.clone());
                                this.toasts.set_action_label(Some("Undo"));
                                this.toasts.show(StateKind::Success, &message);
                            }
                        }
                        FileJobOutcome::Cancelled => {
                            this.set_status(&format!(
                                "Cancelled after undoing {completed} step{}.",
                                plural(completed)
                            ));
                        }
                    }
                }
            },
        );
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        self.pending_toast_undo.borrow_mut().take();
        self.toasts.set_action_label(None);
        let kind = classify_status(text);
        if matches!(kind, StateKind::Error | StateKind::Success) {
            self.toasts.show(kind, text);
        }
    }

    fn show_selected_path(&self) {
        let selected = self.selected_file_items();
        self.show_selection_preview(&selected);
        let Some(item) = selected.first() else {
            self.path_entry
                .set_text(&self.current_location.borrow().display_text());
            return;
        };
        if selected.len() == 1 {
            self.path_entry.set_text(&item.display_path());
        } else {
            self.path_entry
                .set_text(&self.current_location.borrow().display_text());
        }
    }

    fn show_selection_preview(&self, selected: &[FileItem]) {
        let generation = self.preview_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.preview_picture.set_visible(false);
        self.preview_picture.set_filename(Option::<&Path>::None);
        self.preview_text.set_text("");

        let Some(item) = selected.first() else {
            self.preview_title.set_text("Select a file");
            self.preview_metadata.set_text("");
            return;
        };
        if selected.len() > 1 {
            self.preview_title
                .set_text(&format!("{} items selected", selected.len()));
            let total = selected
                .iter()
                .filter(|item| !item.is_dir)
                .map(|item| item.size)
                .sum::<u64>();
            self.preview_metadata
                .set_text(&format!("Combined file size: {}", format_size(total)));
            return;
        }

        self.preview_title.set_text(&item.name);
        self.preview_metadata.set_text(&format!(
            "{}\n{}\nModified {}\n{}",
            if item.is_dir {
                "Folder".to_string()
            } else {
                format_size(item.size)
            },
            item.content_type,
            item.modified,
            item.display_path()
        ));
        if item.is_dir {
            self.preview_text.set_text("Folder preview");
            return;
        }

        let request = thumbnail_request(
            &item.uri,
            item.path.as_deref(),
            &item.content_type,
            item.size,
        );
        let text_path = item
            .path
            .clone()
            .filter(|_| is_text_preview(&item.content_type, item.size));
        if request.is_none() && text_path.is_none() {
            self.preview_text.set_text(if item.path.is_none() {
                "Preview unavailable for remote files."
            } else if item.content_type.starts_with("image/")
                || item.content_type == "application/pdf"
            {
                "Preview unavailable because this format is unsupported or exceeds the safe preview limit."
            } else {
                "No preview available for this file type."
            });
            return;
        }

        self.preview_text.set_text("Loading preview…");
        let (sender, receiver) = mpsc::channel();
        let active_generation = self.preview_generation.clone();
        thread::spawn(move || {
            if active_generation.load(Ordering::Acquire) != generation {
                return;
            }
            let result = if let Some(request) = request {
                let result = generate_thumbnail(&request)
                    .map(PreviewResult::Picture)
                    .unwrap_or(PreviewResult::Unavailable);
                prune_cache();
                result
            } else if let Some(path) = text_path {
                read_text_preview(&path)
                    .map(PreviewResult::Text)
                    .unwrap_or(PreviewResult::Unavailable)
            } else {
                PreviewResult::Unavailable
            };
            if active_generation.load(Ordering::Acquire) == generation {
                let _ = sender.send(result);
            }
        });

        let active_generation = self.preview_generation.clone();
        let picture = self.preview_picture.clone();
        let text = self.preview_text.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(25), move || {
            if active_generation.load(Ordering::Acquire) != generation {
                return ControlFlow::Break;
            }
            match receiver.try_recv() {
                Ok(PreviewResult::Picture(path)) => {
                    picture.set_filename(Some(path));
                    picture.set_visible(true);
                    text.set_text("");
                    ControlFlow::Break
                }
                Ok(PreviewResult::Text(preview)) => {
                    text.set_text(&preview);
                    ControlFlow::Break
                }
                Ok(PreviewResult::Unavailable) => {
                    text.set_text("Preview could not be generated safely.");
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            }
        });
    }

    fn start_thumbnail_jobs(&self, generation: u64, jobs: Vec<GridThumbnailJob>) {
        if jobs.is_empty() {
            return;
        }
        let requests = jobs
            .iter()
            .map(|job| job.request.clone())
            .collect::<Vec<_>>();
        let (sender, receiver) = mpsc::channel();
        let active_generation = self.thumbnail_generation.clone();
        thread::spawn(move || {
            for request in requests {
                if active_generation.load(Ordering::Acquire) != generation {
                    break;
                }
                if let Ok(path) = generate_thumbnail(&request) {
                    if sender.send((request.uri, path)).is_err() {
                        break;
                    }
                }
            }
            prune_cache();
        });

        let active_generation = self.thumbnail_generation.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(25), move || {
            if active_generation.load(Ordering::Acquire) != generation {
                return ControlFlow::Break;
            }
            loop {
                match receiver.try_recv() {
                    Ok((uri, path)) => {
                        if let Some(job) = jobs.iter().find(|job| job.request.uri == uri) {
                            job.picture.set_filename(Some(path));
                            job.stack.set_visible_child_name("thumbnail");
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => return ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => return ControlFlow::Break,
                }
            }
        });
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
        } else {
            match open_file(&item.file) {
                Ok(()) => {
                    if load_settings().privacy.recent_files {
                        if let Err(err) = remember_recent_file(&item.uri) {
                            info!(
                                target: "focaldesk",
                                session_id = session_id(),
                                error = %err,
                                "failed to remember recent file"
                            );
                        }
                    }
                }
                Err(err) => self.set_status(&format!("Could not open {}: {err}", item.name)),
            }
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

    fn toggle_launcher_favorite(&self, item: &FileItem) {
        match toggle_file_favorite(&item.uri) {
            Ok(true) => self.toasts.show(
                StateKind::Success,
                &format!("Added {} to Launcher Favorites.", item.name),
            ),
            Ok(false) => self.toasts.show(
                StateKind::Success,
                &format!("Removed {} from Launcher Favorites.", item.name),
            ),
            Err(err) => self.toasts.show(
                StateKind::Error,
                &format!("Could not update Launcher Favorites: {err}"),
            ),
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

    fn set_search_query(&self, query: String) {
        let normalized = query.trim().to_string();
        if *self.search_query.borrow() == normalized {
            return;
        }

        *self.search_query.borrow_mut() = normalized;
        self.context_menu_index.borrow_mut().take();
        self.restart_recursive_search();
    }

    fn set_recursive_search(&self, enabled: bool) {
        if *self.recursive_search_enabled.borrow() == enabled {
            return;
        }

        *self.recursive_search_enabled.borrow_mut() = enabled;
        self.context_menu_index.borrow_mut().take();
        self.restart_recursive_search();
    }

    fn recursive_search_active(&self) -> bool {
        *self.recursive_search_enabled.borrow()
            && !self.search_query.borrow().is_empty()
            && matches!(&*self.current_location.borrow(), Location::Path(_))
    }

    fn restart_recursive_search(&self) {
        self.search_generation.fetch_add(1, Ordering::AcqRel);
        if let Some(source_id) = self.pending_search.borrow_mut().take() {
            source_id.remove();
        }
        self.recursive_search_result.borrow_mut().take();

        if !self.recursive_search_active() {
            self.render_entries();
            return;
        }

        self.render_entries();
        let generation = self.search_generation.load(Ordering::Acquire);
        let root = match &*self.current_location.borrow() {
            Location::Path(path) => path.clone(),
            Location::Trash | Location::Uri(_) | Location::Separator => return,
        };
        let query = self.search_query.borrow().clone();
        let show_hidden = self.hidden_toggle.is_active();
        let this = self.clone();
        let pending_search = self.pending_search.clone();
        let source_id = glib::timeout_add_local_once(RECURSIVE_SEARCH_DEBOUNCE, move || {
            pending_search.borrow_mut().take();
            this.start_recursive_search(root, query, show_hidden, generation);
        });
        self.pending_search.borrow_mut().replace(source_id);
    }

    fn start_recursive_search(
        &self,
        root: PathBuf,
        query: String,
        show_hidden: bool,
        generation: u64,
    ) {
        let search_generation = self.search_generation.clone();
        let worker_generation = search_generation.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = recursive_filename_search(
                &root,
                &query,
                show_hidden,
                RECURSIVE_SEARCH_LIMITS,
                || worker_generation.load(Ordering::Acquire) != generation,
            );
            let _ = sender.send(result);
        });

        let this = self.clone();
        glib::timeout_add_local(RECURSIVE_SEARCH_POLL_INTERVAL, move || {
            if search_generation.load(Ordering::Acquire) != generation {
                return ControlFlow::Break;
            }
            match receiver.try_recv() {
                Ok(output) => {
                    if !output.cancelled {
                        *this.recursive_search_result.borrow_mut() =
                            Some(recursive_search_view(output));
                        this.render_entries();
                    }
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    this.set_status("Subfolder search ended unexpectedly.");
                    ControlFlow::Break
                }
            }
        });
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

    fn show_location_properties_dialog(&self, location: &Location) {
        let (title, header_text, rows) = match location {
            Location::Trash => {
                let items = match read_location_items(location, true) {
                    Ok(items) => items,
                    Err(err) => {
                        self.set_status(&format!("Could not read Trash properties: {err}"));
                        return;
                    }
                };
                let total_size = items.iter().map(|item| item.size).sum::<u64>();
                (
                    "Trash Properties".to_string(),
                    "Trash".to_string(),
                    vec![
                        ("Type", "Trash".to_string()),
                        (
                            "Contains",
                            format!("{} item{}", items.len(), plural(items.len())),
                        ),
                        ("Size", format_size(total_size)),
                        ("URI", "trash:///".to_string()),
                    ],
                )
            }
            Location::Path(path) => {
                let file = gio::File::for_path(path);
                let info = match file.query_info(
                    "standard::display-name,standard::type,time::modified",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                ) {
                    Ok(info) => info,
                    Err(err) => {
                        self.set_status(&format!(
                            "Could not read properties for {}: {err}",
                            path.display()
                        ));
                        return;
                    }
                };
                let items = match read_location_items(location, true) {
                    Ok(items) => items,
                    Err(err) => {
                        self.set_status(&format!(
                            "Could not read properties for {}: {err}",
                            path.display()
                        ));
                        return;
                    }
                };
                let name = info.display_name().to_string();
                let parent = path
                    .parent()
                    .map(|parent| parent.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                (
                    format!("{name} Properties"),
                    name,
                    vec![
                        ("Type", "Folder".to_string()),
                        ("Location", parent),
                        (
                            "Contains",
                            format!("{} item{}", items.len(), plural(items.len())),
                        ),
                        ("Modified", modified_text(&info)),
                        ("Path", path.to_string_lossy().into_owned()),
                    ],
                )
            }
            Location::Uri(uri) => {
                let items = match read_location_items(location, true) {
                    Ok(items) => items,
                    Err(err) => {
                        self.set_status(&format!("Could not read properties for {uri}: {err}"));
                        return;
                    }
                };
                (
                    "Location Properties".to_string(),
                    uri.clone(),
                    vec![
                        ("Type", "Remote Location".to_string()),
                        (
                            "Contains",
                            format!("{} item{}", items.len(), plural(items.len())),
                        ),
                        ("URI", uri.clone()),
                    ],
                )
            }
            Location::Separator => return,
        };

        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(&title)
            .default_width(460)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let header = gtk::Label::new(Some(&header_text));
        header.set_xalign(0.0);
        header.set_wrap(true);
        header.add_css_class("title-3");
        content.append(&header);

        let grid = gtk::Grid::new();
        grid.set_column_spacing(12);
        grid.set_row_spacing(8);
        let mut row = 0;
        for (label, value) in rows {
            add_property_row(&grid, &mut row, label, value);
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
        let local_paths = items
            .iter()
            .map(|item| item.path.clone())
            .collect::<Option<Vec<_>>>();
        let local_permission_snapshots = local_paths.as_ref().and_then(|paths| {
            if paths.len() > MAX_PERMISSION_ITEMS {
                None
            } else {
                permission_snapshots(paths).ok()
            }
        });
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
            add_property_row(&grid, &mut row, "Path", path.to_string_lossy());
        } else {
            add_property_row(&grid, &mut row, "URI", items[0].uri.clone());
        }
        if !is_multiple {
            if let Some(original) = &items[0].trash_original_path {
                add_property_row(
                    &grid,
                    &mut row,
                    "Original Location",
                    original.to_string_lossy(),
                );
            }
        }
        if let Some(snapshots) = &local_permission_snapshots {
            add_property_row(
                &grid,
                &mut row,
                "Owner",
                permission_owner_summary(snapshots),
            );
            add_property_row(
                &grid,
                &mut row,
                "Group",
                permission_group_summary(snapshots),
            );
            add_property_row(&grid, &mut row, "Mode", permission_mode_summary(snapshots));
        }

        content.append(&grid);

        let apply_permissions = match (local_paths, local_permission_snapshots) {
            (Some(paths), Some(snapshots)) => {
                Some(self.append_permission_editor(&content, &dialog, paths, snapshots))
            }
            (Some(paths), None) if paths.len() > MAX_PERMISSION_ITEMS => {
                let note = gtk::Label::new(Some(&format!(
                    "Permission editing is limited to {MAX_PERMISSION_ITEMS} selected items."
                )));
                note.set_xalign(0.0);
                note.set_wrap(true);
                note.add_css_class("dim-label");
                content.append(&note);
                None
            }
            (Some(_), None) => {
                let note = gtk::Label::new(Some(
                    "Permissions are unavailable for one or more selected items.",
                ));
                note.set_xalign(0.0);
                note.set_wrap(true);
                note.add_css_class("dim-label");
                content.append(&note);
                None
            }
            (None, _) => None,
        };

        let close = gtk::Button::with_label("Close");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&close);
        if let Some(apply) = apply_permissions {
            apply.add_css_class("suggested-action");
            actions.append(&apply);
        } else {
            close.add_css_class("suggested-action");
        }
        content.append(&actions);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&content));
        dialog.set_child(Some(&scroller));

        let dialog_for_close = dialog.clone();
        close.connect_clicked(move |_| dialog_for_close.close());
        dialog.present();
    }

    fn append_permission_editor(
        &self,
        content: &gtk::Box,
        dialog: &gtk::Window,
        paths: Vec<PathBuf>,
        snapshots: Vec<PermissionSnapshot>,
    ) -> gtk::Button {
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let heading = gtk::Label::new(Some("Permissions"));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        content.append(&heading);

        let permission_grid = gtk::Grid::new();
        permission_grid.set_column_spacing(16);
        permission_grid.set_row_spacing(6);
        for (column, label) in ["Owner", "Group", "Others"].iter().enumerate() {
            let label = gtk::Label::new(Some(label));
            label.add_css_class("dim-label");
            permission_grid.attach(&label, column as i32 + 1, 0, 1, 1);
        }
        let mut buttons = Vec::with_capacity(9);
        for (row, (label, masks)) in [
            ("Read", [0o400, 0o040, 0o004]),
            ("Write", [0o200, 0o020, 0o002]),
            ("Execute", [0o100, 0o010, 0o001]),
        ]
        .iter()
        .enumerate()
        {
            let label = gtk::Label::new(Some(label));
            label.set_xalign(0.0);
            permission_grid.attach(&label, 0, row as i32 + 1, 1, 1);
            for (column, mask) in masks.iter().enumerate() {
                let button = gtk::CheckButton::new();
                let first = snapshots[0].mode & mask != 0;
                let mixed = snapshots
                    .iter()
                    .any(|snapshot| (snapshot.mode & mask != 0) != first);
                button.set_active(first);
                button.set_inconsistent(mixed);
                permission_grid.attach(&button, column as i32 + 1, row as i32 + 1, 1, 1);
                buttons.push(button);
            }
        }
        content.append(&permission_grid);

        let all_files = snapshots.iter().all(|snapshot| snapshot.is_file);
        let executable = gtk::CheckButton::with_label("Allow executing as a program");
        executable.set_visible(all_files);
        if all_files {
            let first = snapshots[0].mode & 0o111 != 0;
            let mixed = snapshots
                .iter()
                .any(|snapshot| (snapshot.mode & 0o111 != 0) != first);
            executable.set_active(first);
            executable.set_inconsistent(mixed);
        }
        content.append(&executable);

        let recursive = gtk::CheckButton::with_label("Apply to enclosed files and folders");
        recursive.set_visible(snapshots.iter().any(|snapshot| snapshot.is_dir));
        recursive.set_tooltip_text(Some(
            "Does not follow symbolic links or cross into mounted filesystems",
        ));
        content.append(&recursive);

        let preview = gtk::Label::new(None);
        preview.set_xalign(0.0);
        preview.set_wrap(true);
        preview.add_css_class("dim-label");
        content.append(&preview);
        let apply = gtk::Button::with_label("Apply Permissions");

        let syncing_execute = Rc::new(Cell::new(false));
        let refresh: Rc<dyn Fn()> = Rc::new({
            let buttons = buttons.clone();
            let snapshots = snapshots.clone();
            let recursive = recursive.clone();
            let preview = preview.clone();
            let apply = apply.clone();
            move || {
                let changes = permission_changes_from_buttons(&buttons);
                let modes = snapshots
                    .iter()
                    .map(|snapshot| apply_permission_bits(snapshot.mode, changes))
                    .collect::<HashSet<_>>();
                let mut labels = modes
                    .into_iter()
                    .map(|mode| format!("{} ({:04o})", symbolic_mode(mode), mode & 0o7777))
                    .collect::<Vec<_>>();
                labels.sort();
                preview.set_text(&format!("Result: {}", labels.join(", ")));
                let direct_change = snapshots
                    .iter()
                    .any(|snapshot| apply_permission_bits(snapshot.mode, changes) != snapshot.mode);
                let recursive_change =
                    recursive.is_active() && changes.bits.iter().any(Option::is_some);
                apply.set_sensitive(direct_change || recursive_change);
            }
        });

        for button in &buttons {
            let refresh = refresh.clone();
            let button = button.clone();
            let signal_button = button.clone();
            signal_button.connect_toggled(move |_| {
                if button.is_inconsistent() {
                    button.set_inconsistent(false);
                }
                refresh();
            });
        }
        for index in [6_usize, 7, 8] {
            let buttons = buttons.clone();
            let executable = executable.clone();
            let syncing = syncing_execute.clone();
            let refresh = refresh.clone();
            let signal_button = buttons[index].clone();
            signal_button.connect_toggled(move |_| {
                if syncing.get() {
                    return;
                }
                syncing.set(true);
                let states = [6_usize, 7, 8].map(|index| {
                    (
                        !buttons[index].is_inconsistent(),
                        buttons[index].is_active(),
                    )
                });
                let all_known = states.iter().all(|(known, _)| *known);
                let all_on = all_known && states.iter().all(|(_, active)| *active);
                let all_off = all_known && states.iter().all(|(_, active)| !*active);
                executable.set_inconsistent(!(all_on || all_off));
                if all_on || all_off {
                    executable.set_active(all_on);
                }
                syncing.set(false);
                refresh();
            });
        }
        let buttons_for_execute = buttons.clone();
        let syncing = syncing_execute.clone();
        let refresh_for_execute = refresh.clone();
        executable.connect_toggled(move |button| {
            if syncing.get() {
                return;
            }
            syncing.set(true);
            if button.is_inconsistent() {
                button.set_inconsistent(false);
            }
            for index in [6_usize, 7, 8] {
                buttons_for_execute[index].set_inconsistent(false);
                buttons_for_execute[index].set_active(button.is_active());
            }
            syncing.set(false);
            refresh_for_execute();
        });
        let refresh_for_recursive = refresh.clone();
        recursive.connect_toggled(move |_| refresh_for_recursive());

        let this = self.clone();
        let dialog = dialog.clone();
        apply.connect_clicked({
            let buttons = buttons.clone();
            move |_| {
                let changes = permission_changes_from_buttons(&buttons);
                if recursive.is_active() {
                    this.show_recursive_permissions_confirmation(
                        paths.clone(),
                        changes,
                        dialog.clone(),
                    );
                } else {
                    dialog.close();
                    this.start_permission_change(paths.clone(), changes, false);
                }
            }
        });
        refresh();
        apply
    }

    fn show_recursive_permissions_confirmation(
        &self,
        paths: Vec<PathBuf>,
        changes: PermissionChanges,
        properties_dialog: gtk::Window,
    ) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Apply Permissions Recursively?")
            .default_width(440)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let heading = gtk::Label::new(Some(
            "Change permissions for the selected folders and their contents?",
        ));
        heading.set_xalign(0.0);
        heading.set_wrap(true);
        heading.add_css_class("title-3");
        let warning = gtk::Label::new(Some(
            "This is limited to 10,000 local items. Symbolic links and mounted filesystems are skipped.",
        ));
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::with_label("Apply Recursively");
        apply.add_css_class("destructive-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&apply);
        content.append(&heading);
        content.append(&warning);
        content.append(&actions);
        dialog.set_child(Some(&content));
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let this = self.clone();
        let dialog_for_apply = dialog.clone();
        apply.connect_clicked(move |_| {
            dialog_for_apply.close();
            properties_dialog.close();
            this.start_permission_change(paths.clone(), changes, true);
        });
        dialog.present();
    }

    fn start_permission_change(
        &self,
        paths: Vec<PathBuf>,
        changes: PermissionChanges,
        recursive: bool,
    ) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Applying Permissions")
            .default_width(420)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let label = gtk::Label::new(Some("Validating and applying permission changes…"));
        label.set_xalign(0.0);
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_text(Some("No changes are committed unless validation succeeds"));
        content.append(&label);
        content.append(&progress);
        dialog.set_child(Some(&content));
        dialog.present();

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = apply_permission_changes(&paths, changes, recursive);
            let _ = sender.send(result);
        });
        let this = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            progress.pulse();
            match receiver.try_recv() {
                Ok(Ok(report)) => {
                    dialog.close();
                    this.reload();
                    let message = format!(
                        "Updated permissions for {} item{}.",
                        report.changed,
                        plural(report.changed)
                    );
                    this.remember_undo(
                        "permission change",
                        vec![UndoStep::RestorePermissions {
                            modes: report.previous_modes,
                        }],
                        &message,
                    );
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    dialog.close();
                    this.reload();
                    this.set_status(&format!(
                        "Could not apply permissions: {error}. Applied changes were rolled back."
                    ));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    dialog.close();
                    this.reload();
                    this.set_status("Permission task ended unexpectedly.");
                    ControlFlow::Break
                }
            }
        });
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
        let undo_steps = Rc::new(RefCell::new(Vec::new()));
        let undo_steps_for_job = undo_steps.clone();
        self.run_progress_job(
            "Copying",
            files.len(),
            move |index| match files.get(index) {
                Some(file) => copy_dropped_file(file, &target_dir)
                    .map(|path| {
                        undo_steps_for_job
                            .borrow_mut()
                            .push(UndoStep::RemoveCreated {
                                path,
                                empty_only: false,
                            });
                    })
                    .map_err(|err| err.to_string()),
                None => Ok(()),
            },
            {
                let this = self.clone();
                move |outcome, completed, error| match outcome {
                    FileJobOutcome::Finished => {
                        this.reload();
                        let message = match (completed, error) {
                            (0, Some(err)) => format!("Could not copy dropped files: {err}"),
                            (count, Some(err)) => format!(
                                "Copied {count} item{}; some items failed: {err}",
                                plural(count)
                            ),
                            (count, None) => {
                                format!("Copied {count} dropped item{}.", plural(count))
                            }
                        };
                        this.remember_undo("copy", undo_steps.borrow().clone(), &message);
                    }
                    FileJobOutcome::Cancelled => {
                        this.remember_undo(
                            "copy",
                            undo_steps.borrow().clone(),
                            &format!(
                                "Cancelled after copying {completed} item{}.",
                                plural(completed)
                            ),
                        );
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

fn recursive_filename_search(
    root: &Path,
    query: &str,
    show_hidden: bool,
    limits: SearchLimits,
    is_cancelled: impl Fn() -> bool,
) -> RecursiveSearchOutput {
    let mut output = RecursiveSearchOutput::default();
    let query = query.to_ascii_lowercase();
    let mut pending = vec![(root.to_path_buf(), 0_usize)];

    while let Some((directory, depth)) = pending.pop() {
        if is_cancelled() {
            output.cancelled = true;
            break;
        }
        if output.scanned_dirs >= limits.max_directories {
            output.directory_limit_reached = true;
            break;
        }
        output.scanned_dirs += 1;

        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                output.skipped_dirs += 1;
                continue;
            }
        };
        for entry in entries {
            if is_cancelled() {
                output.cancelled = true;
                return output;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let file_type = metadata.file_type();
            let is_dir = file_type.is_dir();
            let relative_path = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();

            if name.to_ascii_lowercase().contains(&query) {
                output.matches.push(SearchMatch {
                    path: path.clone(),
                    relative_path,
                    is_dir,
                    size: if is_dir { 0 } else { metadata.len() },
                    modified: metadata.modified().ok(),
                });
                if output.matches.len() >= limits.max_results {
                    output.result_limit_reached = true;
                    return output;
                }
            }

            if is_dir {
                if depth < limits.max_depth {
                    pending.push((path, depth + 1));
                } else {
                    output.depth_limit_reached = true;
                }
            }
        }
    }

    output
}

fn recursive_search_view(output: RecursiveSearchOutput) -> RecursiveSearchView {
    let items = output
        .matches
        .into_iter()
        .map(|search_match| {
            let file = gio::File::for_path(&search_match.path);
            let (content_type, _) = if search_match.is_dir {
                ("inode/directory".into(), false)
            } else {
                gio::content_type_guess(Some(&search_match.path), &[])
            };
            let icon = gio::content_type_get_symbolic_icon(&content_type);
            let name = search_match
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            FileItem {
                name,
                relative_path: Some(search_match.relative_path),
                trash_original_path: None,
                uri: file.uri().to_string(),
                path: Some(search_match.path),
                file,
                is_dir: search_match.is_dir,
                size: search_match.size,
                modified: system_time_text(search_match.modified),
                content_type: content_type.to_string(),
                icon,
            }
        })
        .collect();

    RecursiveSearchView {
        items,
        scanned_dirs: output.scanned_dirs,
        skipped_dirs: output.skipped_dirs,
        result_limit_reached: output.result_limit_reached,
        directory_limit_reached: output.directory_limit_reached,
        depth_limit_reached: output.depth_limit_reached,
    }
}

fn read_file_items(file: &gio::File, show_hidden: bool) -> Result<Vec<FileItem>, glib::Error> {
    let enumerator = file.enumerate_children(
        "standard::name,standard::display-name,standard::type,standard::size,time::modified,\
         standard::icon,standard::symbolic-icon,standard::content-type,trash::orig-path",
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
            relative_path: None,
            trash_original_path: info
                .attribute_byte_string("trash::orig-path")
                .map(|path| PathBuf::from(path.as_str())),
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

    let name = gtk::Label::new(Some(item.display_name()));
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

    let name = gtk::Label::new(Some(item.display_name()));
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

fn grid_file_child(item: &FileItem) -> (gtk::FlowBoxChild, (gtk::Stack, gtk::Picture)) {
    let child = gtk::FlowBoxChild::new();
    child.add_css_class("file-grid-child");

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 6);
    tile.add_css_class("file-grid-tile");
    tile.set_halign(gtk::Align::Center);

    let icon = gtk::Image::from_gicon(&item.icon);
    icon.set_pixel_size(48);
    let picture = gtk::Picture::new();
    picture.add_css_class("file-grid-thumbnail");
    picture.set_size_request(96, 72);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    let preview = gtk::Stack::new();
    preview.set_transition_type(gtk::StackTransitionType::Crossfade);
    preview.add_named(&icon, Some("icon"));
    preview.add_named(&picture, Some("thumbnail"));
    preview.set_visible_child_name("icon");
    tile.append(&preview);

    let name = gtk::Label::new(Some(item.display_name()));
    name.set_xalign(0.5);
    name.set_justify(gtk::Justification::Center);
    name.set_wrap(true);
    name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    name.set_lines(2);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("file-grid-name");
    tile.append(&name);

    child.set_child(Some(&tile));
    (child, (preview, picture))
}

fn file_context_target(item: &FileItem) -> FileContextTarget {
    if item.is_dir {
        FileContextTarget::Folder
    } else {
        FileContextTarget::File
    }
}

fn launcher_favorite_action_label(uri: &str, is_favorite: bool) -> Option<&'static str> {
    if uri.is_empty() {
        None
    } else if is_favorite {
        Some("Remove from Launcher Favorites")
    } else {
        Some("Add to Launcher Favorites")
    }
}

fn attach_file_context_menu(
    widget: &impl IsA<gtk::Widget>,
    target: FileContextTarget,
    uri: String,
    is_trash: bool,
    is_archive: bool,
    select_item: impl Fn() + 'static,
    toggle_launcher_favorite: impl Fn() + 'static,
    rename_item: impl Fn() + 'static,
) {
    let toggle_launcher_favorite: Rc<dyn Fn()> = Rc::new(toggle_launcher_favorite);
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

            if is_trash {
                menu.append(&file_context_action_button(
                    "Restore",
                    "win.file-restore-from-trash",
                ));
                menu.append(&file_context_action_button(
                    "Delete Permanently…\tShift+Delete",
                    "win.file-delete-permanently",
                ));
                menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            } else {
                if let Some(favorite_label) =
                    launcher_favorite_action_label(&uri, is_file_favorite(&uri))
                {
                    let favorite = gtk::Button::with_label(favorite_label);
                    favorite.add_css_class("flat");
                    favorite.set_halign(gtk::Align::Fill);
                    let toggle_launcher_favorite = toggle_launcher_favorite.clone();
                    let popover_for_favorite = popover.clone();
                    favorite.connect_clicked(move |_| {
                        popover_for_favorite.popdown();
                        toggle_launcher_favorite();
                    });
                    menu.append(&favorite);
                    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                }

                menu.append(&file_context_action_button("Open", "win.file-open"));
                if matches!(target, FileContextTarget::Folder) {
                    menu.append(&file_context_action_button(
                        "Open in New Tab",
                        "win.file-open-tab",
                    ));
                    menu.append(&file_context_action_button(
                        "Add to Places",
                        "win.file-add-bookmark",
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
                if is_archive {
                    menu.append(&file_context_action_button(
                        "Extract Here",
                        "win.file-extract-here",
                    ));
                    menu.append(&file_context_action_button(
                        "Extract To…",
                        "win.file-extract-to",
                    ));
                }
                menu.append(&file_context_action_button(
                    "Move to Trash\tDelete",
                    "win.file-move-to-trash",
                ));
                menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            menu.append(&file_context_action_button(
                "Properties\tAlt+Return",
                "win.file-properties",
            ));

            let menu_scroll = gtk::ScrolledWindow::new();
            menu_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            menu_scroll.set_propagate_natural_width(true);
            menu_scroll.set_propagate_natural_height(true);
            menu_scroll.set_max_content_height(520);
            menu_scroll.set_child(Some(&menu));
            popover.set_child(Some(&menu_scroll));
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
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let Some(place) = item.item().and_downcast::<gtk::StringObject>() else {
            return;
        };

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

        let name = place.string();
        let title = name.lines().next().unwrap_or_default();
        let detail = name.lines().nth(1).unwrap_or_default();
        let route = name.lines().nth(2).unwrap_or(detail);
        let capability = name.lines().nth(3).unwrap_or_default();
        if capability == "bookmark-missing" {
            label.set_text(&format!("{title} (Unavailable)"));
            label.add_css_class("dim-label");
        } else {
            label.set_text(title);
        }
        label.set_tooltip_text(Some(detail));
        attach_sidebar_context_menu(&row, sidebar_kind_for_place(route, capability), route);
        item.set_child(Some(&row));
    });
    factory
}

fn populate_places(places: &gtk::StringList, monitor: &gio::VolumeMonitor) {
    while places.n_items() > 0 {
        places.remove(0);
    }

    let mut paths = vec![
        ("Home", home_dir()),
        ("Documents", home_dir().join("Documents")),
        ("Projects", projects_location()),
        ("Trash", PathBuf::from("trash:///")),
        ("----------", PathBuf::from("-")),
        ("Desktop", home_dir().join("Desktop")),
        ("Music", home_dir().join("Music")),
        ("Pictures", home_dir().join("Pictures")),
        ("Videos", home_dir().join("Videos")),
        ("Downloads", home_dir().join("Downloads")),
    ];
    paths.retain(|(name, path)| {
        *name == "Projects"
            || path == Path::new("trash:///")
            || path == Path::new("-")
            || path.exists()
    });
    for (name, path) in paths {
        let capability = if name == "Projects" { "\nprojects" } else { "" };
        places.append(&format!("{name}\n{}{capability}", path.to_string_lossy()));
    }

    match load_local_bookmarks() {
        Ok(bookmarks) => {
            let bookmarks = bookmarks
                .into_iter()
                .filter(|bookmark| !is_duplicate_standard_shortcut(&bookmark.path))
                .collect::<Vec<_>>();
            if !bookmarks.is_empty() {
                places.append("Places\n-\n@section");
                for bookmark in bookmarks {
                    let title = bookmark
                        .label
                        .clone()
                        .unwrap_or_else(|| local_bookmark_default_name(&bookmark.path));
                    let available = bookmark.path.is_dir();
                    let detail = if available {
                        bookmark.path.to_string_lossy().into_owned()
                    } else {
                        format!("Missing — {}", bookmark.path.to_string_lossy())
                    };
                    places.append(&format!(
                        "{}\n{}\n@bookmark:{}\n{}",
                        single_line(&title),
                        single_line(&detail),
                        bookmark.uri,
                        if available {
                            "bookmark"
                        } else {
                            "bookmark-missing"
                        }
                    ));
                }
            }
        }
        Err(error) => info!(
            target: "focaldesk",
            error = %error,
            "could not load local Places bookmarks"
        ),
    }

    populate_nonlocal_places(places, monitor);
}

fn populate_nonlocal_places(places: &gtk::StringList, monitor: &gio::VolumeMonitor) {
    match load_network_bookmarks() {
        Ok(bookmarks) if !bookmarks.is_empty() => {
            places.append("Network Bookmarks\n-\n@section");
            for uri in bookmarks {
                places.append(&format!(
                    "{}\n{uri}\n@network:{uri}\nbookmark",
                    network_location_title(&uri)
                ));
            }
        }
        Ok(_) => {}
        Err(err) => info!(
            target: "focaldesk",
            error = %err,
            "could not load Files network bookmarks"
        ),
    }

    let devices = device_sidebar_rows(monitor);
    if !devices.is_empty() {
        places.append("Devices\n-\n@section");
        for device in devices {
            places.append(&device);
        }
    }
}

fn device_sidebar_rows(monitor: &gio::VolumeMonitor) -> Vec<String> {
    let mut rows = Vec::new();
    let mut represented_mounts = Vec::new();

    for volume in monitor.volumes() {
        let name = single_line(&volume.name());
        if let Some(mount) = volume.get_mount() {
            let uri = mount.root().uri().to_string();
            represented_mounts.push(uri.clone());
            rows.push(format!(
                "{name}\nMounted — {uri}\n@mount:{uri}\n{}",
                if mount.can_eject() {
                    "eject"
                } else {
                    "unmount"
                }
            ));
        } else if volume.can_mount() || volume.can_eject() {
            let key = volume_key(&volume);
            rows.push(format!(
                "{name}\nNot mounted — click to mount or unlock\n@volume:{key}\n{}",
                if volume.can_eject() { "eject" } else { "mount" }
            ));
        }
    }

    for mount in monitor.mounts() {
        let uri = mount.root().uri().to_string();
        if represented_mounts.contains(&uri) || (!mount.can_unmount() && !mount.can_eject()) {
            continue;
        }
        let name = single_line(&mount.name());
        rows.push(format!(
            "{name}\nMounted — {uri}\n@mount:{uri}\n{}",
            if mount.can_eject() {
                "eject"
            } else {
                "unmount"
            }
        ));
    }

    rows.sort_by_key(|row| row.lines().next().unwrap_or_default().to_lowercase());
    rows
}

fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn volume_key(volume: &gio::Volume) -> String {
    if let Some(uuid) = volume.uuid() {
        return format!("uuid={}", single_line(&uuid));
    }
    if let Some(device) = volume.identifier("unix-device") {
        return format!("device={}", single_line(&device));
    }
    if let Some(root) = volume.activation_root() {
        return format!("root={}", single_line(&root.uri()));
    }
    format!("name={}", single_line(&volume.name()))
}

fn location_for_file(file: &gio::File) -> Location {
    file.path()
        .map(Location::Path)
        .unwrap_or_else(|| Location::Uri(file.uri().to_string()))
}

fn location_from_sidebar_path(path: &str) -> Location {
    match path {
        "trash:///" => Location::Trash,
        "-" => Location::Separator,
        path if path.starts_with("file://") => gio::File::for_uri(path)
            .path()
            .map(Location::Path)
            .unwrap_or_else(|| Location::Uri(path.to_string())),
        path if path.contains("://") => Location::Uri(path.to_string()),
        path => Location::Path(PathBuf::from(path)),
    }
}

fn validate_network_uri(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a server address.".to_string());
    }
    if input.len() > MAX_NETWORK_URI_BYTES || input.contains(['\n', '\r', '\0']) {
        return Err("The server address is too long or contains invalid characters.".to_string());
    }

    let uri = glib::Uri::parse(
        input,
        glib::UriFlags::HAS_PASSWORD | glib::UriFlags::SCHEME_NORMALIZE,
    )
    .map_err(|_| "Enter a valid server address.".to_string())?;
    let scheme = uri.scheme().to_ascii_lowercase();
    if !matches!(
        scheme.as_str(),
        "smb" | "sftp" | "ftp" | "ftps" | "dav" | "davs"
    ) {
        return Err("Use an SMB, SFTP, FTP, FTPS, WebDAV, or secure WebDAV address.".to_string());
    }
    if uri.host().is_none_or(|host| host.is_empty()) {
        return Err("The server address must include a host name.".to_string());
    }
    let userinfo_contains_password_separator = uri.userinfo().is_some_and(|userinfo| {
        let userinfo = userinfo.to_ascii_lowercase();
        userinfo.contains(':') || userinfo.contains("%3a")
    });
    if uri.password().is_some()
        || uri.auth_params().is_some()
        || userinfo_contains_password_separator
    {
        return Err(
            "Do not put a password in the address; Files will request it securely.".to_string(),
        );
    }
    if uri.query().is_some() || uri.fragment().is_some() {
        return Err("Server addresses cannot contain query parameters or fragments.".to_string());
    }
    Ok(uri.to_str().to_string())
}

fn network_connection_error(error: &glib::Error) -> String {
    if error.matches(gio::IOErrorEnum::PermissionDenied)
        || error.matches(gio::IOErrorEnum::ProxyAuthFailed)
        || error.matches(gio::IOErrorEnum::ProxyNeedAuth)
    {
        "Could not connect to server: login was denied. Check the username and password."
            .to_string()
    } else if error.matches(gio::IOErrorEnum::HostNotFound) {
        "Could not connect to server: the host name was not found.".to_string()
    } else if error.matches(gio::IOErrorEnum::NetworkUnreachable)
        || error.matches(gio::IOErrorEnum::HostUnreachable)
        || error.matches(gio::IOErrorEnum::NotConnected)
    {
        "Could not connect to server: the network or host is offline.".to_string()
    } else if error.matches(gio::IOErrorEnum::ConnectionRefused) {
        "Could not connect to server: the server refused the connection.".to_string()
    } else if error.matches(gio::IOErrorEnum::TimedOut) {
        "Could not connect to server: the connection timed out.".to_string()
    } else if error.matches(gio::TlsError::BadCertificate)
        || error.matches(gio::TlsError::CertificateRequired)
        || error.matches(gio::TlsError::Handshake)
    {
        "Could not connect to server: the secure connection or certificate was rejected."
            .to_string()
    } else {
        format!("Could not connect to server: {error}")
    }
}

fn network_location_title(uri: &str) -> String {
    let Ok(uri) = glib::Uri::parse(uri, glib::UriFlags::HAS_PASSWORD) else {
        return "Network Server".to_string();
    };
    let host = uri
        .host()
        .map(|host| single_line(&host))
        .unwrap_or_else(|| "Network Server".to_string());
    let path = uri.path();
    let share = path
        .split('/')
        .find(|part| !part.is_empty())
        .map(single_line);
    match share {
        Some(share) => format!("{share} on {host}"),
        None => host,
    }
}

fn gtk_bookmarks_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("gtk-3.0")
        .join("bookmarks")
}

fn parse_bookmark_line(line: &str) -> Option<(&str, Option<&str>)> {
    if line.is_empty() || line.len() > MAX_BOOKMARK_LINE_BYTES || line.contains(['\0', '\n', '\r'])
    {
        return None;
    }
    let (uri, label) = match line.split_once(' ') {
        Some((uri, label)) => (uri, Some(label.trim())),
        None => (line, None),
    };
    if uri.is_empty() || uri.chars().any(char::is_whitespace) {
        return None;
    }
    Some((uri, label.filter(|label| !label.is_empty())))
}

fn local_bookmark_path(uri: &str) -> Option<PathBuf> {
    let parsed = glib::Uri::parse(uri, glib::UriFlags::NONE).ok()?;
    if !parsed.scheme().eq_ignore_ascii_case("file") {
        return None;
    }
    let path = gio::File::for_uri(uri).path()?;
    path.is_absolute().then_some(path)
}

fn local_bookmark_from_line(line: &str) -> Option<LocalBookmark> {
    let (uri, label) = parse_bookmark_line(line)?;
    Some(LocalBookmark {
        uri: uri.to_string(),
        path: local_bookmark_path(uri)?,
        label: label.map(single_line),
    })
}

fn local_bookmark_default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn load_bookmark_lines_from(path: &Path) -> io::Result<Vec<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GTK bookmarks path is not a regular file",
        ));
    }
    if metadata.len() > MAX_BOOKMARK_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GTK bookmarks file is too large",
        ));
    }

    let contents = std::fs::read_to_string(path)?;
    let lines = contents
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    if lines.len() > MAX_BOOKMARK_LINES
        || lines
            .iter()
            .any(|line| line.len() > MAX_BOOKMARK_LINE_BYTES || line.contains(['\0', '\n', '\r']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GTK bookmarks file exceeds its safety limits",
        ));
    }
    Ok(lines)
}

fn load_local_bookmarks_from(path: &Path) -> io::Result<Vec<LocalBookmark>> {
    let mut seen = HashSet::new();
    Ok(load_bookmark_lines_from(path)?
        .iter()
        .filter_map(|line| local_bookmark_from_line(line))
        .filter(|bookmark| seen.insert(bookmark.uri.clone()))
        .take(MAX_LOCAL_BOOKMARKS)
        .collect())
}

fn load_local_bookmarks() -> io::Result<Vec<LocalBookmark>> {
    load_local_bookmarks_from(&gtk_bookmarks_path())
}

fn validate_bookmark_label(label: &str) -> io::Result<String> {
    let label = label.trim();
    if label.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter a bookmark name.",
        ));
    }
    if label.len() > MAX_BOOKMARK_LABEL_BYTES || label.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bookmark names must be short and cannot contain control characters.",
        ));
    }
    Ok(label.to_string())
}

fn save_bookmark_lines_to(path: &Path, lines: &[String]) -> io::Result<()> {
    if lines.len() > MAX_BOOKMARK_LINES
        || lines
            .iter()
            .any(|line| line.len() > MAX_BOOKMARK_LINE_BYTES || line.contains(['\0', '\n', '\r']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GTK bookmarks exceed their safety limits",
        ));
    }
    let total_bytes = lines
        .iter()
        .try_fold(0_u64, |total, line| {
            total.checked_add(line.len() as u64 + 1)
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bookmark size overflow"))?;
    if total_bytes > MAX_BOOKMARK_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GTK bookmarks file is too large",
        ));
    }

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "bookmark path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temporary = None;
    for attempt in 0..100 {
        let candidate = parent.join(format!(
            ".bookmarks.{}.{stamp}.{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a bookmark temporary file",
        )
    })?;
    let result = (|| {
        for line in lines {
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        std::fs::rename(&temporary_path, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary_path);
    }
    result
}

fn add_local_bookmark_to(bookmarks_path: &Path, folder: &Path) -> io::Result<bool> {
    let folder = normalize_path(folder);
    if !folder.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the bookmark target is not an available local folder",
        ));
    }
    let uri = gio::File::for_path(&folder).uri().to_string();
    let mut lines = load_bookmark_lines_from(bookmarks_path)?;
    let local_bookmarks = lines
        .iter()
        .filter_map(|line| local_bookmark_from_line(line))
        .collect::<Vec<_>>();
    if local_bookmarks
        .iter()
        .any(|bookmark| bookmark.uri == uri || bookmark.path == folder)
    {
        return Ok(false);
    }
    if local_bookmarks.len() >= MAX_LOCAL_BOOKMARKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Places is limited to {MAX_LOCAL_BOOKMARKS} local bookmarks"),
        ));
    }
    lines.push(uri);
    save_bookmark_lines_to(bookmarks_path, &lines)?;
    Ok(true)
}

fn add_local_bookmark(folder: &Path) -> io::Result<bool> {
    add_local_bookmark_to(&gtk_bookmarks_path(), folder)
}

fn rename_local_bookmark_in(bookmarks_path: &Path, uri: &str, label: &str) -> io::Result<bool> {
    let label = validate_bookmark_label(label)?;
    let mut lines = load_bookmark_lines_from(bookmarks_path)?;
    let Some(index) = lines.iter().position(|line| {
        parse_bookmark_line(line).is_some_and(|(candidate, _)| candidate == uri)
            && local_bookmark_path(uri).is_some()
    }) else {
        return Ok(false);
    };
    lines[index] = format!("{uri} {label}");
    save_bookmark_lines_to(bookmarks_path, &lines)?;
    Ok(true)
}

fn rename_local_bookmark(uri: &str, label: &str) -> io::Result<bool> {
    rename_local_bookmark_in(&gtk_bookmarks_path(), uri, label)
}

fn remove_local_bookmark_from(bookmarks_path: &Path, uri: &str) -> io::Result<bool> {
    if local_bookmark_path(uri).is_none() {
        return Ok(false);
    }
    let mut lines = load_bookmark_lines_from(bookmarks_path)?;
    let original_len = lines.len();
    lines.retain(|line| parse_bookmark_line(line).is_none_or(|(candidate, _)| candidate != uri));
    if lines.len() == original_len {
        return Ok(false);
    }
    save_bookmark_lines_to(bookmarks_path, &lines)?;
    Ok(true)
}

fn remove_local_bookmark(uri: &str) -> io::Result<bool> {
    remove_local_bookmark_from(&gtk_bookmarks_path(), uri)
}

fn move_local_bookmark_in(
    bookmarks_path: &Path,
    uri: &str,
    direction: BookmarkDirection,
) -> io::Result<bool> {
    if local_bookmark_path(uri).is_none() {
        return Ok(false);
    }
    let mut lines = load_bookmark_lines_from(bookmarks_path)?;
    let local_indices = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| local_bookmark_from_line(line).map(|_| index))
        .collect::<Vec<_>>();
    let Some(position) = local_indices.iter().position(|index| {
        parse_bookmark_line(&lines[*index]).is_some_and(|(candidate, _)| candidate == uri)
    }) else {
        return Ok(false);
    };
    let destination = match direction {
        BookmarkDirection::Up => position.checked_sub(1),
        BookmarkDirection::Down if position + 1 < local_indices.len() => Some(position + 1),
        BookmarkDirection::Down => None,
    };
    let Some(destination) = destination else {
        return Ok(false);
    };
    lines.swap(local_indices[position], local_indices[destination]);
    save_bookmark_lines_to(bookmarks_path, &lines)?;
    Ok(true)
}

fn move_local_bookmark(uri: &str, direction: BookmarkDirection) -> io::Result<bool> {
    move_local_bookmark_in(&gtk_bookmarks_path(), uri, direction)
}

fn network_bookmarks_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("focaldesk")
        .join("files-bookmarks")
}

fn load_network_bookmarks() -> io::Result<Vec<String>> {
    load_network_bookmarks_from(&network_bookmarks_path())
}

fn load_network_bookmarks_from(path: &Path) -> io::Result<Vec<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bookmark path is not a regular file",
        ));
    }
    if metadata.len() > (MAX_NETWORK_BOOKMARKS * (MAX_NETWORK_URI_BYTES + 1)) as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bookmark file is too large",
        ));
    }

    let contents = std::fs::read_to_string(path)?;
    let mut bookmarks = Vec::new();
    for line in contents.lines().take(MAX_NETWORK_BOOKMARKS) {
        if let Ok(uri) = validate_network_uri(line) {
            if !bookmarks.contains(&uri) {
                bookmarks.push(uri);
            }
        }
    }
    Ok(bookmarks)
}

fn add_network_bookmark(uri: &str) -> io::Result<bool> {
    let uri = validate_network_uri(uri)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let path = network_bookmarks_path();
    let mut bookmarks = load_network_bookmarks_from(&path)?;
    if bookmarks.contains(&uri) {
        return Ok(false);
    }
    if bookmarks.len() >= MAX_NETWORK_BOOKMARKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many server bookmarks",
        ));
    }
    bookmarks.push(uri);
    save_network_bookmarks_to(&path, &bookmarks)?;
    Ok(true)
}

fn remove_network_bookmark(uri: &str) -> io::Result<bool> {
    let path = network_bookmarks_path();
    let mut bookmarks = load_network_bookmarks_from(&path)?;
    let original_len = bookmarks.len();
    bookmarks.retain(|bookmark| bookmark != uri);
    if bookmarks.len() == original_len {
        return Ok(false);
    }
    save_network_bookmarks_to(&path, &bookmarks)?;
    Ok(true)
}

fn save_network_bookmarks_to(path: &Path, bookmarks: &[String]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "bookmark path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temporary = None;
    for attempt in 0..100 {
        let candidate = parent.join(format!(
            ".files-bookmarks.{}.{stamp}.{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a bookmark temporary file",
        )
    })?;
    let result = (|| {
        for uri in bookmarks.iter().take(MAX_NETWORK_BOOKMARKS) {
            let uri = validate_network_uri(uri)
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
            file.write_all(uri.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        std::fs::rename(&temporary_path, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary_path);
    }
    result
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

fn copy_dropped_file(file: &gio::File, target_dir: &Path) -> io::Result<PathBuf> {
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
        copy_path_recursively(&source, &destination)?;
    } else {
        let destination_file = gio::File::for_path(&destination);
        file.copy(
            &destination_file,
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|err| io::Error::other(err.to_string()))?;
    }
    Ok(destination)
}

fn move_clipboard_file(file: &gio::File, target_dir: &Path) -> io::Result<PathBuf> {
    let name = file.basename().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard file has no basename",
        )
    })?;
    let destination = available_destination(target_dir, Path::new(&name));

    if let Some(source) = file.path() {
        if source == destination {
            return Ok(destination);
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
        })?;
    } else {
        let destination_file = gio::File::for_path(&destination);
        file.move_(
            &destination_file,
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|err| io::Error::other(err.to_string()))?;
    }
    Ok(destination)
}

fn bulk_rename_mode(index: u32) -> BulkRenameMode {
    match index {
        1 => BulkRenameMode::Prefix,
        2 => BulkRenameMode::Suffix,
        3 => BulkRenameMode::Number,
        4 => BulkRenameMode::Lowercase,
        5 => BulkRenameMode::Uppercase,
        _ => BulkRenameMode::FindReplace,
    }
}

fn configure_bulk_rename_fields(
    mode: BulkRenameMode,
    primary_label: &gtk::Label,
    primary: &gtk::Entry,
    secondary_label: &gtk::Label,
    secondary: &gtk::Entry,
) {
    let (
        primary_text,
        primary_hint,
        secondary_text,
        secondary_hint,
        primary_enabled,
        secondary_enabled,
    ) = match mode {
        BulkRenameMode::FindReplace => (
            "Find",
            "Text to find",
            "Replace with",
            "Replacement text",
            true,
            true,
        ),
        BulkRenameMode::Prefix => ("Prefix", "Text before each name", "", "", true, false),
        BulkRenameMode::Suffix => ("Suffix", "Text after each name", "", "", true, false),
        BulkRenameMode::Number => ("Prefix", "File ", "Start number", "1", true, true),
        BulkRenameMode::Lowercase | BulkRenameMode::Uppercase => ("", "", "", "", false, false),
    };
    primary_label.set_text(primary_text);
    primary_label.set_visible(primary_enabled);
    primary.set_placeholder_text(Some(primary_hint));
    primary.set_visible(primary_enabled);
    secondary_label.set_text(secondary_text);
    secondary_label.set_visible(secondary_enabled);
    secondary.set_placeholder_text(Some(secondary_hint));
    secondary.set_visible(secondary_enabled);
}

fn bulk_rename_preview_row(old_name: &str, new_name: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.set_margin_top(5);
    content.set_margin_bottom(5);
    content.set_margin_start(8);
    content.set_margin_end(8);
    let old = gtk::Label::new(Some(old_name));
    old.set_xalign(0.0);
    old.set_hexpand(true);
    old.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    old.add_css_class("dim-label");
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    let new = gtk::Label::new(Some(new_name));
    new.set_xalign(0.0);
    new.set_hexpand(true);
    new.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    content.append(&old);
    content.append(&arrow);
    content.append(&new);
    row.set_child(Some(&content));
    row
}

fn build_bulk_rename_plan(
    paths: &[PathBuf],
    options: &BulkRenameOptions,
) -> Result<BulkRenamePlan, String> {
    if paths.len() < 2 {
        return Err("Select at least two items for bulk rename.".to_string());
    }
    if paths.len() > MAX_BULK_RENAME_ITEMS {
        return Err(format!(
            "Bulk rename is limited to {MAX_BULK_RENAME_ITEMS} items."
        ));
    }
    if matches!(options.mode, BulkRenameMode::FindReplace) && options.primary.is_empty() {
        return Err("Enter text to find.".to_string());
    }
    let start_number = if matches!(options.mode, BulkRenameMode::Number) {
        options
            .secondary
            .trim()
            .parse::<u64>()
            .map_err(|_| "Start number must be a non-negative whole number.".to_string())?
    } else {
        0
    };
    let final_number = start_number
        .checked_add(paths.len().saturating_sub(1) as u64)
        .ok_or_else(|| "The numbering range is too large.".to_string())?;
    let number_width = final_number.to_string().len().max(3);

    let parent = paths[0]
        .parent()
        .ok_or_else(|| "A selected item has no parent folder.".to_string())?
        .to_path_buf();
    let source_set = paths.iter().cloned().collect::<HashSet<_>>();
    if source_set.len() != paths.len() {
        return Err("The selection contains the same item more than once.".to_string());
    }

    let mut mappings = Vec::with_capacity(paths.len());
    let mut moves = Vec::new();
    let mut destinations = HashSet::with_capacity(paths.len());
    for (index, source) in paths.iter().enumerate() {
        if source.parent() != Some(parent.as_path()) {
            return Err("Bulk rename requires items from the same folder.".to_string());
        }
        let old_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "A selected filename is not valid UTF-8.".to_string())?;
        let is_dir = std::fs::symlink_metadata(source)
            .map_err(|error| format!("Could not inspect {old_name}: {error}"))?
            .is_dir();
        let (stem, extension) = split_rename_name(old_name, is_dir, options.preserve_extension);
        let renamed_stem = match options.mode {
            BulkRenameMode::FindReplace => stem.replace(&options.primary, &options.secondary),
            BulkRenameMode::Prefix => format!("{}{stem}", options.primary),
            BulkRenameMode::Suffix => format!("{stem}{}", options.primary),
            BulkRenameMode::Number => format!(
                "{}{:0width$}",
                options.primary,
                start_number + index as u64,
                width = number_width
            ),
            BulkRenameMode::Lowercase => stem.to_lowercase(),
            BulkRenameMode::Uppercase => stem.to_uppercase(),
        };
        let new_name = format!("{renamed_stem}{extension}");
        validate_filename(&new_name)?;
        let destination = parent.join(&new_name);
        if !destinations.insert(destination.clone()) {
            return Err(format!("More than one item would be named “{new_name}”."));
        }
        if path_lexists(&destination) && !source_set.contains(&destination) {
            return Err(format!("An item named “{new_name}” already exists."));
        }
        mappings.push((old_name.to_string(), new_name));
        if destination != *source {
            moves.push((source.clone(), destination));
        }
    }

    if moves.is_empty() {
        return Err("No names would change.".to_string());
    }
    Ok(BulkRenamePlan { mappings, moves })
}

fn split_rename_name(name: &str, is_dir: bool, preserve_extension: bool) -> (&str, &str) {
    if is_dir || !preserve_extension {
        return (name, "");
    }
    let path = Path::new(name);
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return (name, "");
    };
    if extension.is_empty() || extension.len() + 1 > name.len() {
        return (name, "");
    }
    let split = name.len() - extension.len() - 1;
    (&name[..split], &name[split..])
}

fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() || matches!(name, "." | "..") {
        return Err("Generated names cannot be empty, “.”, or “..”.".to_string());
    }
    if name.contains('/') || name.contains('\0') {
        return Err(format!("“{name}” contains an invalid character."));
    }
    if name.len() > 255 {
        return Err(format!("“{name}” exceeds the 255-byte filename limit."));
    }
    Ok(())
}

fn execute_rename_transaction(moves: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    let sources = moves
        .iter()
        .map(|(source, _)| source.clone())
        .collect::<HashSet<_>>();
    let mut destinations = HashSet::with_capacity(moves.len());
    for (source, destination) in moves {
        if !path_lexists(source) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} no longer exists", source.display()),
            ));
        }
        if !destinations.insert(destination.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("duplicate destination {}", destination.display()),
            ));
        }
        if path_lexists(destination) && !sources.contains(destination) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", destination.display()),
            ));
        }
    }

    let mut temporary_moves = Vec::with_capacity(moves.len());
    let mut reserved = sources.clone();
    reserved.extend(destinations.iter().cloned());
    for (index, (source, _)) in moves.iter().enumerate() {
        let parent = source.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "rename source has no parent")
        })?;
        let temporary = unique_rename_temporary_path(parent, index, &reserved);
        reserved.insert(temporary.clone());
        temporary_moves.push((source.clone(), temporary));
    }

    for (staged, (source, temporary)) in temporary_moves.iter().enumerate() {
        if let Err(error) = move_no_replace(source, temporary) {
            let rollback = rollback_staged_renames(&temporary_moves[..staged]).err();
            return Err(rename_transaction_error(error, rollback));
        }
    }

    for (index, (_, destination)) in moves.iter().enumerate() {
        if let Err(error) = move_no_replace(&temporary_moves[index].1, destination) {
            let mut rollback_error = None;
            for completed in (0..index).rev() {
                if let Err(rollback) =
                    move_no_replace(&moves[completed].1, &temporary_moves[completed].1)
                {
                    rollback_error = Some(rollback);
                    break;
                }
            }
            if rollback_error.is_none() {
                rollback_error = rollback_staged_renames(&temporary_moves).err();
            }
            return Err(rename_transaction_error(error, rollback_error));
        }
    }
    Ok(())
}

fn unique_rename_temporary_path(
    parent: &Path,
    index: usize,
    reserved: &HashSet<PathBuf>,
) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".focaldesk-rename-{}-{counter}-{index}",
            std::process::id()
        ));
        if !path_lexists(&candidate) && !reserved.contains(&candidate) {
            return candidate;
        }
    }
}

fn move_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    if path_lexists(destination) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display()),
        ));
    }
    gio::File::for_path(source)
        .move_(
            &gio::File::for_path(destination),
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|error| io::Error::other(error.to_string()))
}

fn rollback_staged_renames(staged: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    for (source, temporary) in staged.iter().rev() {
        if path_lexists(temporary) {
            move_no_replace(temporary, source)?;
        }
    }
    Ok(())
}

fn rename_transaction_error(error: io::Error, rollback: Option<io::Error>) -> io::Error {
    match rollback {
        Some(rollback) => io::Error::other(format!("{error}; rollback also failed: {rollback}")),
        None => error,
    }
}

fn path_lexists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn permission_snapshot(path: &Path) -> io::Result<PermissionSnapshot> {
    let metadata = std::fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("symbolic links are not changed: {}", path.display()),
        ));
    }
    if !file_type.is_file() && !file_type.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("special files are not changed: {}", path.display()),
        ));
    }
    Ok(PermissionSnapshot {
        path: path.to_path_buf(),
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
        device: metadata.dev(),
        inode: metadata.ino(),
        is_dir: file_type.is_dir(),
        is_file: file_type.is_file(),
    })
}

fn permission_snapshots(paths: &[PathBuf]) -> io::Result<Vec<PermissionSnapshot>> {
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no local items were selected",
        ));
    }
    if paths.len() > MAX_PERMISSION_ITEMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("permission editing is limited to {MAX_PERMISSION_ITEMS} selected items"),
        ));
    }
    paths.iter().map(|path| permission_snapshot(path)).collect()
}

fn account_name(database: &Path, id: u32, id_column: usize) -> Option<String> {
    let contents = std::fs::read_to_string(database).ok()?;
    contents.lines().find_map(|line| {
        let fields = line.split(':').collect::<Vec<_>>();
        (fields.len() > id_column && fields[id_column].parse::<u32>().ok() == Some(id))
            .then(|| single_line(fields[0]))
    })
}

fn permission_owner_summary(snapshots: &[PermissionSnapshot]) -> String {
    let owners = snapshots
        .iter()
        .map(|snapshot| snapshot.uid)
        .collect::<HashSet<_>>();
    if owners.len() != 1 {
        return "Multiple owners".to_string();
    }
    let uid = *owners.iter().next().expect("one owner");
    match account_name(Path::new("/etc/passwd"), uid, 2) {
        Some(name) => format!("{name} ({uid})"),
        None => format!("UID {uid}"),
    }
}

fn permission_group_summary(snapshots: &[PermissionSnapshot]) -> String {
    let groups = snapshots
        .iter()
        .map(|snapshot| snapshot.gid)
        .collect::<HashSet<_>>();
    if groups.len() != 1 {
        return "Multiple groups".to_string();
    }
    let gid = *groups.iter().next().expect("one group");
    match account_name(Path::new("/etc/group"), gid, 2) {
        Some(name) => format!("{name} ({gid})"),
        None => format!("GID {gid}"),
    }
}

fn permission_mode_summary(snapshots: &[PermissionSnapshot]) -> String {
    let modes = snapshots
        .iter()
        .map(|snapshot| snapshot.mode)
        .collect::<HashSet<_>>();
    if modes.len() != 1 {
        return "Mixed permissions".to_string();
    }
    let mode = *modes.iter().next().expect("one mode");
    format!("{} ({mode:04o})", symbolic_mode(mode))
}

fn symbolic_mode(mode: u32) -> String {
    let mut output = String::with_capacity(9);
    for (read, write, execute, special, special_on, special_off) in [
        (0o400, 0o200, 0o100, 0o4000, 's', 'S'),
        (0o040, 0o020, 0o010, 0o2000, 's', 'S'),
        (0o004, 0o002, 0o001, 0o1000, 't', 'T'),
    ] {
        output.push(if mode & read != 0 { 'r' } else { '-' });
        output.push(if mode & write != 0 { 'w' } else { '-' });
        output.push(match (mode & execute != 0, mode & special != 0) {
            (true, true) => special_on,
            (false, true) => special_off,
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    output
}

fn permission_changes_from_buttons(buttons: &[gtk::CheckButton]) -> PermissionChanges {
    debug_assert_eq!(buttons.len(), PERMISSION_MASKS.len());
    PermissionChanges {
        bits: std::array::from_fn(|index| {
            let button = &buttons[index];
            (!button.is_inconsistent()).then(|| button.is_active())
        }),
    }
}

fn apply_permission_bits(mode: u32, changes: PermissionChanges) -> u32 {
    changes
        .bits
        .iter()
        .zip(PERMISSION_MASKS)
        .fold(mode, |mode, (change, mask)| match change {
            Some(true) => mode | mask,
            Some(false) => mode & !mask,
            None => mode,
        })
}

fn collect_permission_targets(
    paths: &[PathBuf],
    recursive: bool,
) -> io::Result<Vec<PermissionSnapshot>> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = VecDeque::new();
    for path in paths {
        let snapshot = permission_snapshot(path)?;
        pending.push_back((snapshot, true));
    }

    while let Some((snapshot, selected_root)) = pending.pop_front() {
        let identity = (snapshot.device, snapshot.inode);
        if !seen.insert(identity) {
            continue;
        }
        if targets.len() >= MAX_RECURSIVE_PERMISSION_ITEMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "recursive permission changes are limited to {MAX_RECURSIVE_PERMISSION_ITEMS} items"
                ),
            ));
        }
        let root_device = snapshot.device;
        let directory = recursive && snapshot.is_dir;
        let directory_path = snapshot.path.clone();
        targets.push(snapshot);
        if !directory {
            continue;
        }

        let mut children = std::fs::read_dir(&directory_path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let child = permission_snapshot(&entry.path());
            match child {
                Ok(child) if child.device == root_device => pending.push_back((child, false)),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    // Symbolic links and special files are deliberately skipped.
                }
                Err(error) => return Err(error),
            }
        }

        if !selected_root {
            // Descendants retain their own device as the traversal boundary. Because
            // mount points were filtered above, it is the same as the selected root.
            debug_assert!(targets
                .last()
                .is_some_and(|target| target.device == root_device));
        }
    }
    Ok(targets)
}

fn set_mode_no_symlink(path: &Path, mode: u32) -> io::Result<()> {
    let snapshot = permission_snapshot(path)?;
    let mut permissions = std::fs::metadata(&snapshot.path)?.permissions();
    permissions.set_mode(mode & 0o7777);
    std::fs::set_permissions(&snapshot.path, permissions)
}

fn set_permission_modes_with<F>(
    requests: &[(PathBuf, u32)],
    mut setter: F,
) -> io::Result<Vec<(PathBuf, u32)>>
where
    F: FnMut(&Path, u32) -> io::Result<()>,
{
    let previous = requests
        .iter()
        .map(|(path, _)| permission_snapshot(path).map(|snapshot| (path.clone(), snapshot.mode)))
        .collect::<io::Result<Vec<_>>>()?;
    for (applied, (path, mode)) in requests.iter().enumerate() {
        if let Err(error) = setter(path, *mode) {
            let mut rollback_error = None;
            for (rollback_path, rollback_mode) in previous[..applied].iter().rev() {
                if let Err(error) = setter(rollback_path, *rollback_mode) {
                    rollback_error.get_or_insert(error);
                }
            }
            return Err(rename_transaction_error(error, rollback_error));
        }
    }
    Ok(previous)
}

fn apply_permission_changes(
    paths: &[PathBuf],
    changes: PermissionChanges,
    recursive: bool,
) -> io::Result<PermissionChangeReport> {
    let targets = collect_permission_targets(paths, recursive)?;
    let requests = targets
        .iter()
        .filter_map(|snapshot| {
            let mode = apply_permission_bits(snapshot.mode, changes);
            (mode != snapshot.mode).then(|| (snapshot.path.clone(), mode))
        })
        .collect::<Vec<_>>();
    let previous_modes = set_permission_modes_with(&requests, set_mode_no_symlink)?;
    Ok(PermissionChangeReport {
        changed: previous_modes.len(),
        previous_modes,
    })
}

fn restore_permission_modes(modes: &[(PathBuf, u32)]) -> io::Result<()> {
    set_permission_modes_with(modes, set_mode_no_symlink).map(|_| ())
}

fn execute_undo_step(step: &UndoStep) -> io::Result<()> {
    match step {
        UndoStep::MoveBack { from, to } => {
            if !from.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} no longer exists", from.display()),
                ));
            }
            if to.query_exists(gio::Cancellable::NONE) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", to.uri()),
                ));
            }
            if let Some(to) = to.path() {
                std::fs::rename(from, &to).or_else(|_| {
                    copy_path_recursively(from, &to)?;
                    remove_path_recursively(from)
                })
            } else {
                gio::File::for_path(from)
                    .move_(to, gio::FileCopyFlags::NONE, gio::Cancellable::NONE, None)
                    .map_err(|err| io::Error::other(err.to_string()))
            }
        }
        UndoStep::BulkRename { moves } => execute_rename_transaction(moves),
        UndoStep::RemoveCreated { path, empty_only } => {
            if !path.exists() {
                return Ok(());
            }
            if *empty_only {
                std::fs::remove_dir(path)
            } else {
                remove_path_recursively(path)
            }
        }
        UndoStep::RemoveExtracted { paths } => {
            for path in paths.iter().rev() {
                let result = match std::fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                        std::fs::remove_dir(path)
                    }
                    Ok(_) => std::fs::remove_file(path),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "could not remove extracted path {}: {error}",
                            path.display()
                        ),
                    ));
                }
            }
            Ok(())
        }
        UndoStep::RestoreReplacedCopy {
            destination,
            backup,
        } => restore_replaced_copy(destination, backup),
        UndoStep::RestoreReplacedMove {
            source,
            destination,
            backup,
        } => restore_replaced_move(source, destination, backup),
        UndoStep::RestorePermissions { modes } => restore_permission_modes(modes),
        UndoStep::RestoreTrash { original } => restore_trashed_path(original),
    }
}

fn restore_replaced_copy(destination: &Path, backup: &Path) -> io::Result<()> {
    if !path_lexists(destination) || !path_lexists(backup) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the replacement or its undo backup is no longer available",
        ));
    }
    let displaced = transfer_undo_staging_path(destination)?;
    std::fs::rename(destination, &displaced)?;
    if let Err(error) = std::fs::rename(backup, destination) {
        let rollback = std::fs::rename(&displaced, destination);
        return Err(rename_transaction_error(error, rollback.err()));
    }
    remove_path(&displaced)
}

fn restore_replaced_move(source: &Path, destination: &Path, backup: &Path) -> io::Result<()> {
    if path_lexists(source) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", source.display()),
        ));
    }
    if !path_lexists(destination) || !path_lexists(backup) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the moved item or its replacement backup is no longer available",
        ));
    }
    move_path_exact(destination, source)?;
    if let Err(error) = std::fs::rename(backup, destination) {
        let rollback = move_path_exact(source, destination);
        return Err(rename_transaction_error(error, rollback.err()));
    }
    Ok(())
}

fn move_path_exact(source: &Path, destination: &Path) -> io::Result<()> {
    if path_lexists(destination) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display()),
        ));
    }
    std::fs::rename(source, destination).or_else(|_| {
        gio::File::for_path(source)
            .move_(
                &gio::File::for_path(destination),
                gio::FileCopyFlags::NONE,
                gio::Cancellable::NONE,
                None,
            )
            .map_err(|error| io::Error::other(error.to_string()))
    })
}

fn transfer_undo_staging_path(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "undo path has no parent"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..1000 {
        let candidate = parent.join(format!(
            ".focaldesk-transfer-undo-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        if !path_lexists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an undo staging path",
    ))
}

fn cleanup_undo_record(record: &UndoRecord) {
    for step in &record.steps {
        let backup = match step {
            UndoStep::RestoreReplacedCopy { backup, .. }
            | UndoStep::RestoreReplacedMove { backup, .. } => Some(backup),
            _ => None,
        };
        if let Some(backup) = backup {
            if let Err(error) = remove_path(backup) {
                info!(
                    target: "focaldesk",
                    path = %backup.display(),
                    error = %error,
                    "could not clean expired transfer undo backup"
                );
            }
        }
    }
}

fn restore_trashed_path(original: &Path) -> io::Result<()> {
    let trash = gio::File::for_uri("trash:///");
    let enumerator = trash
        .enumerate_children(
            "standard::name,trash::orig-path,trash::deletion-date",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .map_err(|err| io::Error::other(err.to_string()))?;
    let mut newest = None::<(String, gio::File)>;
    while let Some(info) = enumerator
        .next_file(gio::Cancellable::NONE)
        .map_err(|err| io::Error::other(err.to_string()))?
    {
        let Some(original_path) = info.attribute_byte_string("trash::orig-path") else {
            continue;
        };
        if Path::new(original_path.as_str()) != original {
            continue;
        }
        let deletion_date = info
            .attribute_string("trash::deletion-date")
            .map(|value| value.to_string())
            .unwrap_or_default();
        if newest
            .as_ref()
            .is_none_or(|(current_date, _)| deletion_date > *current_date)
        {
            newest = Some((deletion_date, enumerator.child(&info)));
        }
    }

    let Some((_, trashed)) = newest else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("could not find {} in Trash", original.display()),
        ));
    };
    restore_trash_item(&trashed, original)
}

fn restore_trash_item(trashed: &gio::File, original: &Path) -> io::Result<()> {
    if original.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", original.display()),
        ));
    }
    let Some(parent) = original.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent folder", original.display()),
        ));
    };
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("original folder {} is unavailable", parent.display()),
        ));
    }

    let destination = gio::File::for_path(original);
    trashed
        .move_(
            &destination,
            gio::FileCopyFlags::NONE,
            gio::Cancellable::NONE,
            None,
        )
        .map_err(|err| io::Error::other(err.to_string()))
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
        .map(|path| location_from_sidebar_path(&path))
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

fn projects_settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("focaldesk")
        .join("files-sidebar.json")
}

fn default_projects_location() -> PathBuf {
    home_dir().join("Projects")
}

fn parse_projects_settings(contents: &str) -> Option<PathBuf> {
    let settings: FilesSidebarSettings = serde_json::from_str(contents).ok()?;
    settings.projects_location.filter(|path| path.is_absolute())
}

fn projects_location() -> PathBuf {
    let path = projects_settings_path();
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return default_projects_location();
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_PROJECTS_SETTINGS_BYTES
    {
        return default_projects_location();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| parse_projects_settings(&contents))
        .unwrap_or_else(default_projects_location)
}

fn save_projects_location(projects_path: &Path) -> io::Result<()> {
    if !projects_path.is_absolute() || !projects_path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Projects must be an existing local folder",
        ));
    }
    let settings_path = projects_settings_path();
    if settings_path.exists()
        && std::fs::symlink_metadata(&settings_path)?
            .file_type()
            .is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Projects settings is not a regular file",
        ));
    }
    let parent = settings_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Projects settings has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let contents = serde_json::to_vec_pretty(&FilesSidebarSettings {
        projects_location: Some(projects_path.to_path_buf()),
    })
    .map_err(io::Error::other)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let (temporary_path, mut temporary) = (0..100)
        .find_map(|attempt| {
            let candidate = parent.join(format!(
                ".files-sidebar.{}.{stamp}.{attempt}.tmp",
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "could not write settings"))?;
    let result = (|| {
        temporary.write_all(&contents)?;
        temporary.sync_all()?;
        std::fs::rename(&temporary_path, &settings_path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

fn is_duplicate_standard_shortcut(path: &Path) -> bool {
    ["Music", "Pictures", "Videos", "Downloads"]
        .iter()
        .any(|name| path == home_dir().join(name))
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

fn system_time_text(time: Option<std::time::SystemTime>) -> String {
    let Some(seconds) = time
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
    else {
        return "-".to_string();
    };
    glib::DateTime::from_unix_local(seconds)
        .and_then(|date| date.format("%Y-%m-%d %H:%M"))
        .map(|text| text.to_string())
        .unwrap_or_else(|_| "-".to_string())
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

fn format_transfer_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn transfer_undo_step(undo: TransferUndo) -> UndoStep {
    match undo {
        TransferUndo::Created { path } => UndoStep::RemoveCreated {
            path,
            empty_only: false,
        },
        TransferUndo::Moved {
            source,
            destination,
        } => UndoStep::MoveBack {
            from: destination,
            to: gio::File::for_path(source),
        },
        TransferUndo::ReplacedCopy {
            destination,
            backup,
        } => UndoStep::RestoreReplacedCopy {
            destination,
            backup,
        },
        TransferUndo::ReplacedMove {
            source,
            destination,
            backup,
        } => UndoStep::RestoreReplacedMove {
            source,
            destination,
            backup,
        },
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

fn sidebar_kind_for_place(route: &str, capability: &str) -> SidebarKind {
    if capability == "projects" {
        return SidebarKind::Projects;
    }
    match route {
        "trash:///" => SidebarKind::Trash,
        "-" | "@section" => SidebarKind::Separator,
        route if route.starts_with("@mount:") => SidebarKind::MountedDevice {
            can_eject: capability == "eject",
        },
        route if route.starts_with("@volume:") => SidebarKind::UnmountedVolume {
            can_eject: capability == "eject",
        },
        route if route.starts_with("@network:") => SidebarKind::NetworkBookmark,
        route if route.starts_with("@bookmark:") => SidebarKind::UserBookmark {
            available: capability != "bookmark-missing",
        },
        _ => SidebarKind::Folder,
    }
}

fn attach_sidebar_context_menu(widget: &impl IsA<gtk::Widget>, kind: SidebarKind, route: &str) {
    if matches!(kind, SidebarKind::Separator) {
        return;
    }

    let menu = gio::Menu::new();
    let target = match kind {
        SidebarKind::MountedDevice { .. } => route.trim_start_matches("@mount:"),
        SidebarKind::UnmountedVolume { .. } => route.trim_start_matches("@volume:"),
        SidebarKind::NetworkBookmark => route.trim_start_matches("@network:"),
        SidebarKind::UserBookmark { .. } => route.trim_start_matches("@bookmark:"),
        _ => route,
    }
    .to_variant();

    let open_action = match kind {
        SidebarKind::UnmountedVolume { .. } => "win.device-open",
        SidebarKind::NetworkBookmark => "win.network-open",
        SidebarKind::UserBookmark { .. } => "win.bookmark-open",
        _ => "win.sidebar-open",
    };
    menu.append_item(&sidebar_menu_item("Open", open_action, &target));

    if matches!(kind, SidebarKind::Folder | SidebarKind::Projects) {
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
    if matches!(kind, SidebarKind::UserBookmark { available: true }) {
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

        match kind {
            SidebarKind::Projects => {
                section.append(
                    Some("Change Projects Location…"),
                    Some("win.projects-location"),
                );
            }
            SidebarKind::MountedDevice { can_eject: true } => section.append_item(
                &sidebar_menu_item("Eject", "win.device-eject-mount", &target),
            ),
            SidebarKind::MountedDevice { can_eject: false } => {
                section.append_item(&sidebar_menu_item("Unmount", "win.device-unmount", &target))
            }
            SidebarKind::UnmountedVolume { can_eject: true } => section.append_item(
                &sidebar_menu_item("Eject", "win.device-eject-volume", &target),
            ),
            SidebarKind::NetworkBookmark => section.append_item(&sidebar_menu_item(
                "Remove Bookmark",
                "win.network-bookmark-remove",
                &target,
            )),
            SidebarKind::UserBookmark { .. } => {
                section.append_item(&sidebar_menu_item(
                    "Rename…",
                    "win.bookmark-rename",
                    &target,
                ));
                section.append_item(&sidebar_menu_item(
                    "Move Up",
                    "win.bookmark-move-up",
                    &target,
                ));
                section.append_item(&sidebar_menu_item(
                    "Move Down",
                    "win.bookmark-move-down",
                    &target,
                ));
                section.append_item(&sidebar_menu_item(
                    "Remove from Places",
                    "win.bookmark-remove",
                    &target,
                ));
            }
            SidebarKind::Folder
            | SidebarKind::Trash
            | SidebarKind::Separator
            | SidebarKind::UnmountedVolume { can_eject: false } => {}
        }

        section
    });

    menu.append_section(None, &{
        let section = gio::Menu::new();
        if matches!(
            kind,
            SidebarKind::Folder
                | SidebarKind::Projects
                | SidebarKind::Trash
                | SidebarKind::UserBookmark { available: true }
        ) {
            section.append_item(&sidebar_menu_item(
                "Properties",
                "win.sidebar-properties",
                &target,
            ));
        }
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

    fn test_search_limits() -> SearchLimits {
        SearchLimits {
            max_results: 100,
            max_directories: 100,
            max_depth: 10,
        }
    }

    #[test]
    fn recursive_search_finds_nested_names_and_reports_relative_paths() {
        let root = unique_test_dir("recursive-search");
        std::fs::create_dir_all(root.join("projects/archive")).expect("create nested folders");
        std::fs::write(root.join("projects/archive/meeting-notes.txt"), "notes")
            .expect("write nested file");
        std::fs::write(root.join("unrelated.txt"), "other").expect("write unrelated file");

        let output =
            recursive_filename_search(&root, "MEETING", false, test_search_limits(), || false);

        assert!(!output.cancelled);
        assert_eq!(output.matches.len(), 1);
        assert_eq!(
            output.matches[0].relative_path,
            "projects/archive/meeting-notes.txt"
        );
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn recursive_search_hides_hidden_trees_and_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("recursive-search-hidden");
        std::fs::create_dir_all(root.join("visible")).expect("create visible folder");
        std::fs::create_dir_all(root.join(".private")).expect("create hidden folder");
        std::fs::write(root.join(".private/secret-match.txt"), "secret")
            .expect("write hidden file");
        symlink(&root, root.join("visible/loop")).expect("create symlink loop");

        let hidden =
            recursive_filename_search(&root, "match", false, test_search_limits(), || false);
        assert!(hidden.matches.is_empty());
        assert!(hidden.scanned_dirs <= 2);

        let shown = recursive_filename_search(&root, "match", true, test_search_limits(), || false);
        assert_eq!(shown.matches.len(), 1);
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn recursive_search_is_bounded_and_cancellable() {
        let root = unique_test_dir("recursive-search-limits");
        std::fs::create_dir_all(&root).expect("create test folder");
        for index in 0..5 {
            std::fs::write(root.join(format!("match-{index}.txt")), "match")
                .expect("write test file");
        }

        let limited = recursive_filename_search(
            &root,
            "match",
            false,
            SearchLimits {
                max_results: 2,
                ..test_search_limits()
            },
            || false,
        );
        assert_eq!(limited.matches.len(), 2);
        assert!(limited.result_limit_reached);

        std::fs::create_dir_all(root.join("one/two")).expect("create deep folders");
        let depth_limited = recursive_filename_search(
            &root,
            "absent",
            false,
            SearchLimits {
                max_depth: 0,
                ..test_search_limits()
            },
            || false,
        );
        assert!(depth_limited.depth_limit_reached);

        let directory_limited = recursive_filename_search(
            &root,
            "absent",
            false,
            SearchLimits {
                max_directories: 1,
                ..test_search_limits()
            },
            || false,
        );
        assert!(directory_limited.directory_limit_reached);

        let cancelled =
            recursive_filename_search(&root, "match", false, test_search_limits(), || true);
        assert!(cancelled.cancelled);
        assert!(cancelled.matches.is_empty());
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn trash_restore_preserves_conflicts_and_requires_the_original_folder() {
        let root = unique_test_dir("trash-restore");
        let trash = root.join("trash");
        let original_folder = root.join("original");
        std::fs::create_dir_all(&trash).expect("create mock trash");
        std::fs::create_dir_all(&original_folder).expect("create original folder");

        let trashed = trash.join("note.txt");
        let original = original_folder.join("note.txt");
        std::fs::write(&trashed, "restored").expect("write mock trashed file");
        restore_trash_item(&gio::File::for_path(&trashed), &original).expect("restore file");
        assert_eq!(
            std::fs::read_to_string(&original).expect("read restored file"),
            "restored"
        );

        let conflict_source = trash.join("conflict.txt");
        std::fs::write(&conflict_source, "from trash").expect("write conflict source");
        std::fs::write(&original, "keep existing").expect("write existing destination");
        let conflict = restore_trash_item(&gio::File::for_path(&conflict_source), &original);
        assert_eq!(
            conflict
                .expect_err("restore should preserve conflict")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(conflict_source.exists());
        assert_eq!(
            std::fs::read_to_string(&original).expect("read preserved destination"),
            "keep existing"
        );

        let missing_parent_source = trash.join("missing-parent.txt");
        std::fs::write(&missing_parent_source, "still trashed").expect("write source");
        let missing_parent = root.join("removed/folder/file.txt");
        let result = restore_trash_item(
            &gio::File::for_path(&missing_parent_source),
            &missing_parent,
        );
        assert_eq!(
            result.expect_err("restore should require parent").kind(),
            io::ErrorKind::NotFound
        );
        assert!(missing_parent_source.exists());

        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn bulk_rename_preview_preserves_extensions_and_numbers_in_order() {
        let root = unique_test_dir("bulk-rename-preview");
        std::fs::create_dir_all(&root).expect("create root");
        let first = root.join("Summer One.JPG");
        let second = root.join("Summer Two.JPG");
        std::fs::write(&first, "one").expect("write first");
        std::fs::write(&second, "two").expect("write second");

        let prefix = build_bulk_rename_plan(
            &[first.clone(), second.clone()],
            &BulkRenameOptions {
                mode: BulkRenameMode::Prefix,
                primary: "Trip - ".to_string(),
                secondary: String::new(),
                preserve_extension: true,
            },
        )
        .expect("build prefix plan");
        assert_eq!(prefix.mappings[0].1, "Trip - Summer One.JPG");
        assert_eq!(prefix.mappings[1].1, "Trip - Summer Two.JPG");

        let numbered = build_bulk_rename_plan(
            &[first, second],
            &BulkRenameOptions {
                mode: BulkRenameMode::Number,
                primary: "Photo ".to_string(),
                secondary: "7".to_string(),
                preserve_extension: true,
            },
        )
        .expect("build numbering plan");
        assert_eq!(numbered.mappings[0].1, "Photo 007.JPG");
        assert_eq!(numbered.mappings[1].1, "Photo 008.JPG");
        std::fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn bulk_rename_rejects_duplicates_existing_items_and_invalid_names() {
        let root = unique_test_dir("bulk-rename-validation");
        std::fs::create_dir_all(&root).expect("create root");
        let first = root.join("alpha.txt");
        let second = root.join("beta.txt");
        std::fs::write(&first, "alpha").expect("write first");
        std::fs::write(&second, "beta").expect("write second");
        std::fs::write(root.join("taken.txt"), "taken").expect("write conflict");

        let duplicate = build_bulk_rename_plan(
            &[first.clone(), second.clone()],
            &BulkRenameOptions {
                mode: BulkRenameMode::FindReplace,
                primary: "alpha".to_string(),
                secondary: "beta".to_string(),
                preserve_extension: true,
            },
        );
        assert!(duplicate.is_err());

        let conflict = build_bulk_rename_plan(
            &[first.clone(), second.clone()],
            &BulkRenameOptions {
                mode: BulkRenameMode::FindReplace,
                primary: "alpha".to_string(),
                secondary: "taken".to_string(),
                preserve_extension: true,
            },
        );
        assert!(conflict.is_err());

        let invalid = build_bulk_rename_plan(
            &[first, second],
            &BulkRenameOptions {
                mode: BulkRenameMode::Prefix,
                primary: "bad/name".to_string(),
                secondary: String::new(),
                preserve_extension: true,
            },
        );
        assert!(invalid.is_err());
        std::fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn bulk_rename_transaction_supports_swaps_undo_and_failure_rollback() {
        let root = unique_test_dir("bulk-rename-transaction");
        std::fs::create_dir_all(&root).expect("create root");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        std::fs::write(&first, "first contents").expect("write first");
        std::fs::write(&second, "second contents").expect("write second");

        let swap = vec![
            (first.clone(), second.clone()),
            (second.clone(), first.clone()),
        ];
        execute_rename_transaction(&swap).expect("swap names");
        assert_eq!(
            std::fs::read_to_string(&first).expect("read first"),
            "second contents"
        );
        assert_eq!(
            std::fs::read_to_string(&second).expect("read second"),
            "first contents"
        );
        execute_undo_step(&UndoStep::BulkRename {
            moves: swap
                .iter()
                .map(|(source, destination)| (destination.clone(), source.clone()))
                .collect(),
        })
        .expect("undo swap");
        assert_eq!(
            std::fs::read_to_string(&first).expect("read restored first"),
            "first contents"
        );
        assert_eq!(
            std::fs::read_to_string(&second).expect("read restored second"),
            "second contents"
        );

        let final_name = root.join("renamed.txt");
        let missing_parent = root.join("missing/second.txt");
        let failure = execute_rename_transaction(&[
            (first.clone(), final_name.clone()),
            (second.clone(), missing_parent),
        ]);
        assert!(failure.is_err());
        assert_eq!(
            std::fs::read_to_string(&first).expect("read rolled back first"),
            "first contents"
        );
        assert_eq!(
            std::fs::read_to_string(&second).expect("read rolled back second"),
            "second contents"
        );
        assert!(!final_name.exists());
        assert!(std::fs::read_dir(&root)
            .expect("read root")
            .filter_map(Result::ok)
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".focaldesk-rename-")));
        std::fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn sidebar_routes_local_and_remote_locations() {
        assert_eq!(
            location_from_sidebar_path("/media/user/USB"),
            Location::Path(PathBuf::from("/media/user/USB"))
        );
        assert_eq!(
            location_from_sidebar_path("smb://server/share"),
            Location::Uri("smb://server/share".to_string())
        );
        assert_eq!(location_from_sidebar_path("trash:///"), Location::Trash);
        assert_eq!(location_from_sidebar_path("-"), Location::Separator);
        assert_eq!(
            location_from_sidebar_path("file:///home/user/My%20Files"),
            Location::Path(PathBuf::from("/home/user/My Files"))
        );
    }

    #[test]
    fn sidebar_device_routes_expose_the_safe_removal_action() {
        assert_eq!(
            sidebar_kind_for_place("@mount:file:///run/media/user/USB", "eject"),
            SidebarKind::MountedDevice { can_eject: true }
        );
        assert_eq!(
            sidebar_kind_for_place("@mount:file:///mnt/archive", "unmount"),
            SidebarKind::MountedDevice { can_eject: false }
        );
        assert_eq!(
            sidebar_kind_for_place("@volume:uuid=1234", "mount"),
            SidebarKind::UnmountedVolume { can_eject: false }
        );
        assert_eq!(
            sidebar_kind_for_place("@network:smb://server/share", "bookmark"),
            SidebarKind::NetworkBookmark
        );
        assert_eq!(
            sidebar_kind_for_place("@bookmark:file:///work", "bookmark"),
            SidebarKind::UserBookmark { available: true }
        );
        assert_eq!(
            sidebar_kind_for_place("@bookmark:file:///missing", "bookmark-missing"),
            SidebarKind::UserBookmark { available: false }
        );
        assert_eq!(
            sidebar_kind_for_place("/home/user/Projects", "projects"),
            SidebarKind::Projects
        );
    }

    #[test]
    fn projects_sidebar_settings_require_an_absolute_location() {
        assert_eq!(
            parse_projects_settings(r#"{"projects_location":"/work/projects"}"#),
            Some(PathBuf::from("/work/projects"))
        );
        assert_eq!(
            parse_projects_settings(r#"{"projects_location":"relative/projects"}"#),
            None
        );
        assert_eq!(parse_projects_settings("not json"), None);
    }

    #[test]
    fn duplicate_media_shortcuts_are_not_added_as_places_bookmarks() {
        for name in ["Music", "Pictures", "Videos", "Downloads"] {
            assert!(is_duplicate_standard_shortcut(&home_dir().join(name)));
        }
        assert!(!is_duplicate_standard_shortcut(
            &home_dir().join("Projects")
        ));
    }

    #[test]
    fn local_bookmarks_interoperate_and_preserve_unrelated_entries() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = unique_test_dir("local-bookmarks");
        let bookmarks_path = root.join("gtk-3.0/bookmarks");
        let alpha = root.join("Alpha Folder");
        let beta = root.join("Beta");
        std::fs::create_dir_all(&alpha).expect("create alpha folder");
        std::fs::create_dir_all(&beta).expect("create beta folder");
        save_bookmark_lines_to(
            &bookmarks_path,
            &["sftp://files.example.test/home Shared Server".to_string()],
        )
        .expect("seed GTK bookmarks");

        assert!(add_local_bookmark_to(&bookmarks_path, &alpha).expect("add alpha"));
        assert!(add_local_bookmark_to(&bookmarks_path, &beta).expect("add beta"));
        assert!(!add_local_bookmark_to(&bookmarks_path, &alpha).expect("reject duplicate"));
        let alpha_uri = gio::File::for_path(&alpha).uri().to_string();
        let beta_uri = gio::File::for_path(&beta).uri().to_string();
        assert!(
            rename_local_bookmark_in(&bookmarks_path, &alpha_uri, "Work Projects")
                .expect("rename alpha")
        );
        assert!(
            move_local_bookmark_in(&bookmarks_path, &beta_uri, BookmarkDirection::Up)
                .expect("move beta up")
        );

        let bookmarks = load_local_bookmarks_from(&bookmarks_path).expect("load local bookmarks");
        assert_eq!(
            bookmarks
                .iter()
                .map(|bookmark| bookmark.uri.as_str())
                .collect::<Vec<_>>(),
            vec![beta_uri.as_str(), alpha_uri.as_str()]
        );
        assert_eq!(bookmarks[1].label.as_deref(), Some("Work Projects"));
        let lines = load_bookmark_lines_from(&bookmarks_path).expect("load all bookmark lines");
        assert_eq!(lines[0], "sftp://files.example.test/home Shared Server");
        assert_eq!(
            std::fs::metadata(&bookmarks_path)
                .expect("bookmark metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::remove_dir_all(&beta).expect("make beta unavailable");
        let bookmarks =
            load_local_bookmarks_from(&bookmarks_path).expect("reload missing bookmark");
        assert_eq!(bookmarks[0].path, beta);
        assert!(!bookmarks[0].path.exists());
        assert!(remove_local_bookmark_from(&bookmarks_path, &beta_uri).expect("remove beta"));
        assert!(load_bookmark_lines_from(&bookmarks_path)
            .expect("load preserved bookmarks")
            .iter()
            .any(|line| line == "sftp://files.example.test/home Shared Server"));

        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn local_bookmark_storage_rejects_unsafe_files_and_labels() {
        use std::os::unix::fs::symlink;

        assert!(validate_bookmark_label("").is_err());
        assert!(validate_bookmark_label("bad\nname").is_err());

        let root = unique_test_dir("local-bookmark-symlink");
        std::fs::create_dir_all(&root).expect("create root");
        let target = root.join("target");
        let bookmarks = root.join("bookmarks");
        std::fs::write(&target, "file:///tmp\n").expect("write target");
        symlink(&target, &bookmarks).expect("link bookmarks");
        assert!(load_bookmark_lines_from(&bookmarks).is_err());
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn validates_supported_network_locations_without_embedded_secrets() {
        for uri in [
            "smb://server/share",
            "sftp://user@example.test/home/user",
            "ftp://ftp.example.test/pub",
            "ftps://ftp.example.test/pub",
            "dav://cloud.example.test/files",
            "davs://cloud.example.test/files",
        ] {
            assert_eq!(validate_network_uri(uri).as_deref(), Ok(uri));
        }

        assert!(validate_network_uri("https://example.test/files").is_err());
        assert!(validate_network_uri("smb:///share").is_err());
        assert!(validate_network_uri("sftp://user:secret@example.test/home").is_err());
        assert!(validate_network_uri("sftp://user%3Asecret@example.test/home").is_err());
        assert!(validate_network_uri("davs://example.test/files?token=secret").is_err());
        assert!(validate_network_uri("smb://server/share\nftp://other/pub").is_err());
    }

    #[test]
    fn network_bookmarks_are_private_bounded_and_uri_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = unique_test_dir("network-bookmarks");
        let path = root.join("focaldesk/files-bookmarks");
        let bookmarks = vec![
            "smb://server/share".to_string(),
            "sftp://user@example.test/home/user".to_string(),
        ];

        save_network_bookmarks_to(&path, &bookmarks).expect("save bookmarks");
        assert_eq!(
            load_network_bookmarks_from(&path).expect("load bookmarks"),
            bookmarks
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("bookmark metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().expect("bookmark parent"))
                .expect("bookmark parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let invalid = vec!["sftp://user:secret@example.test/home".to_string()];
        assert!(save_network_bookmarks_to(&path, &invalid).is_err());
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn network_bookmark_titles_include_share_and_host() {
        assert_eq!(
            network_location_title("smb://files.example.test/Projects"),
            "Projects on files.example.test"
        );
        assert_eq!(
            network_location_title("sftp://files.example.test/"),
            "files.example.test"
        );
    }

    #[test]
    fn network_errors_are_presented_as_actionable_statuses() {
        let offline = glib::Error::new(gio::IOErrorEnum::NetworkUnreachable, "unreachable");
        assert!(network_connection_error(&offline).contains("offline"));

        let login = glib::Error::new(gio::IOErrorEnum::PermissionDenied, "denied");
        assert!(network_connection_error(&login).contains("username and password"));

        let certificate = glib::Error::new(gio::TlsError::BadCertificate, "bad certificate");
        assert!(network_connection_error(&certificate).contains("certificate"));
    }

    #[test]
    fn sidebar_metadata_is_kept_on_one_line() {
        assert_eq!(single_line("USB\nDrive\r1"), "USB Drive 1");
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
    fn windows_executables_can_be_added_to_launcher_favorites() {
        assert_eq!(
            launcher_favorite_action_label("file:///home/user/Game.exe", false),
            Some("Add to Launcher Favorites")
        );
        assert_eq!(
            launcher_favorite_action_label("file:///home/user/Game.exe", true),
            Some("Remove from Launcher Favorites")
        );
    }

    #[test]
    fn formats_unix_permissions_including_special_bits() {
        assert_eq!(symbolic_mode(0o755), "rwxr-xr-x");
        assert_eq!(symbolic_mode(0o6754), "rwsr-sr--");
        assert_eq!(symbolic_mode(0o1700), "rwx-----T");
    }

    #[test]
    fn permission_changes_preserve_mixed_and_special_bits() {
        let mut bits = [None; 9];
        bits[0] = Some(false);
        bits[8] = Some(true);
        assert_eq!(
            apply_permission_bits(0o4750, PermissionChanges { bits }),
            0o4351
        );
    }

    #[test]
    fn recursive_permission_targets_skip_symlinks() {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("permission-recursion");
        let tree = root.join("tree");
        let nested = tree.join("nested");
        let outside = root.join("outside.txt");
        std::fs::create_dir_all(&nested).expect("create permission tree");
        std::fs::write(nested.join("inside.txt"), "inside").expect("write inside file");
        std::fs::write(&outside, "outside").expect("write outside file");
        symlink(&outside, tree.join("outside-link")).expect("create outside symlink");

        let targets = collect_permission_targets(std::slice::from_ref(&tree), true)
            .expect("collect recursive targets");
        let paths = targets
            .iter()
            .map(|snapshot| snapshot.path.as_path())
            .collect::<HashSet<_>>();
        assert!(paths.contains(tree.as_path()));
        assert!(paths.contains(nested.as_path()));
        assert!(paths.contains(nested.join("inside.txt").as_path()));
        assert!(!paths.contains(tree.join("outside-link").as_path()));
        assert!(!paths.contains(outside.as_path()));

        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn permission_batches_roll_back_and_successful_changes_are_undoable() {
        let root = unique_test_dir("permission-transaction");
        std::fs::create_dir_all(&root).expect("create test folder");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        std::fs::write(&first, "first").expect("write first file");
        std::fs::write(&second, "second").expect("write second file");
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o640))
            .expect("set first mode");
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o600))
            .expect("set second mode");

        let failed_once = Cell::new(false);
        let requests = vec![(first.clone(), 0o700), (second.clone(), 0o700)];
        let result = set_permission_modes_with(&requests, |path, mode| {
            if path == second && !failed_once.replace(true) {
                return Err(io::Error::other("injected failure"));
            }
            set_mode_no_symlink(path, mode)
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::metadata(&first).expect("first metadata").mode() & 0o7777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(&second).expect("second metadata").mode() & 0o7777,
            0o600
        );

        let mut bits = [None; 9];
        bits[6] = Some(true);
        let report = apply_permission_changes(
            std::slice::from_ref(&first),
            PermissionChanges { bits },
            false,
        )
        .expect("apply owner execute permission");
        assert_eq!(
            std::fs::metadata(&first).expect("changed metadata").mode() & 0o7777,
            0o740
        );
        execute_undo_step(&UndoStep::RestorePermissions {
            modes: report.previous_modes,
        })
        .expect("undo permissions");
        assert_eq!(
            std::fs::metadata(&first).expect("restored metadata").mode() & 0o7777,
            0o640
        );

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

    #[test]
    fn copies_selected_file_into_destination() {
        let root = unique_test_dir("copy-to");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        std::fs::create_dir_all(&source_dir).expect("create source folder");
        std::fs::create_dir_all(&target_dir).expect("create target folder");
        let source = source_dir.join("note.txt");
        std::fs::write(&source, "copy me").expect("write source file");

        let copied =
            copy_dropped_file(&gio::File::for_path(&source), &target_dir).expect("copy file");

        assert_eq!(
            std::fs::read_to_string(target_dir.join("note.txt")).expect("read copied file"),
            "copy me"
        );
        assert!(source.exists());
        execute_undo_step(&UndoStep::RemoveCreated {
            path: copied.clone(),
            empty_only: false,
        })
        .expect("undo copy");
        assert!(!copied.exists());
        assert!(source.exists());
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn replacement_transfers_restore_both_sides_on_undo() {
        fn transfer_report(request: TransferRequest) -> transfer::TransferReport {
            let (sender, receiver) = mpsc::channel();
            run_transfer(request, Arc::new(AtomicBool::new(false)), sender);
            receiver
                .into_iter()
                .find_map(|event| match event {
                    TransferEvent::Finished(result) => Some(result.expect("transfer result")),
                    _ => None,
                })
                .expect("finished transfer")
        }

        let root = unique_test_dir("replacement-undo");
        let copy_source_dir = root.join("copy-source");
        let move_source_dir = root.join("move-source");
        let target = root.join("target");
        std::fs::create_dir_all(&copy_source_dir).expect("copy source folder");
        std::fs::create_dir_all(&move_source_dir).expect("move source folder");
        std::fs::create_dir_all(&target).expect("target folder");

        let copy_source = copy_source_dir.join("copy.txt");
        std::fs::write(&copy_source, "new copy").expect("copy source");
        std::fs::write(target.join("copy.txt"), "old copy").expect("copy destination");
        let report = transfer_report(TransferRequest {
            sources: vec![copy_source],
            target_dir: target.clone(),
            operation: TransferOperation::Copy,
            conflict_policy: ConflictPolicy::Replace,
        });
        assert_eq!(
            std::fs::read_to_string(target.join("copy.txt")).expect("replacement copy"),
            "new copy"
        );
        execute_undo_step(&transfer_undo_step(
            report.undo.into_iter().next().expect("copy undo"),
        ))
        .expect("undo replacement copy");
        assert_eq!(
            std::fs::read_to_string(target.join("copy.txt")).expect("restored copy"),
            "old copy"
        );

        let move_source = move_source_dir.join("move.txt");
        std::fs::write(&move_source, "new move").expect("move source");
        std::fs::write(target.join("move.txt"), "old move").expect("move destination");
        let report = transfer_report(TransferRequest {
            sources: vec![move_source.clone()],
            target_dir: target.clone(),
            operation: TransferOperation::Move,
            conflict_policy: ConflictPolicy::Replace,
        });
        assert!(!move_source.exists());
        assert_eq!(
            std::fs::read_to_string(target.join("move.txt")).expect("replacement move"),
            "new move"
        );
        execute_undo_step(&transfer_undo_step(
            report.undo.into_iter().next().expect("move undo"),
        ))
        .expect("undo replacement move");
        assert_eq!(
            std::fs::read_to_string(&move_source).expect("restored move source"),
            "new move"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("move.txt")).expect("restored move target"),
            "old move"
        );
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn moves_selected_folder_into_destination() {
        let root = unique_test_dir("move-to");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        let folder = source_dir.join("documents");
        std::fs::create_dir_all(&folder).expect("create source folder");
        std::fs::create_dir_all(&target_dir).expect("create target folder");
        std::fs::write(folder.join("note.txt"), "move me").expect("write source file");

        let moved =
            move_clipboard_file(&gio::File::for_path(&folder), &target_dir).expect("move folder");

        assert!(!folder.exists());
        assert_eq!(
            std::fs::read_to_string(target_dir.join("documents/note.txt"))
                .expect("read moved file"),
            "move me"
        );
        execute_undo_step(&UndoStep::MoveBack {
            from: moved,
            to: gio::File::for_path(&folder),
        })
        .expect("undo move");
        assert_eq!(
            std::fs::read_to_string(folder.join("note.txt")).expect("read restored file"),
            "move me"
        );
        assert!(!target_dir.join("documents").exists());
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn undo_new_folder_refuses_to_remove_nonempty_folder() {
        let root = unique_test_dir("undo-nonempty-folder");
        let folder = root.join("new folder");
        std::fs::create_dir_all(&folder).expect("create folder");
        std::fs::write(folder.join("keep.txt"), "keep me").expect("write file");

        let result = execute_undo_step(&UndoStep::RemoveCreated {
            path: folder.clone(),
            empty_only: true,
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(folder.join("keep.txt")).expect("read preserved file"),
            "keep me"
        );
        std::fs::remove_dir_all(root).expect("remove test folder");
    }

    #[test]
    fn undo_extraction_removes_created_paths_but_preserves_new_items() {
        let root = unique_test_dir("undo-extraction");
        let extracted = root.join("archive");
        let nested = extracted.join("nested");
        let created_file = nested.join("created.txt");
        std::fs::create_dir_all(&nested).expect("create extraction folders");
        std::fs::write(&created_file, "created").expect("write extracted file");
        let added_later = extracted.join("keep.txt");
        std::fs::write(&added_later, "keep").expect("write later file");

        let result = execute_undo_step(&UndoStep::RemoveExtracted {
            paths: vec![extracted.clone(), nested.clone(), created_file.clone()],
        });

        assert!(result.is_err());
        assert!(!created_file.exists());
        assert!(!nested.exists());
        assert_eq!(
            std::fs::read_to_string(&added_later).expect("read preserved file"),
            "keep"
        );
        std::fs::remove_dir_all(root).expect("remove test folder");
    }
}
