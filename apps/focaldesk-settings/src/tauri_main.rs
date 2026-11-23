//! Incremental Tauri frontend. The existing GTK executable remains available.
use focaldesk_settings::appearance::{self, Appearance, AppearanceSnapshot};
use focaldesk_settings::applications::{
    ApplicationsChange, ApplicationsResult, ApplicationsService, ApplicationsSnapshot,
    AssociationResult, AssociationSnapshot,
};
use focaldesk_settings::calendar::{
    CalendarAction, CalendarPreferences, CalendarResult, CalendarService, CalendarSnapshot,
    ConnectCalendar, PublicFlow,
};
use focaldesk_settings::printers::{
    PrintJob, PrinterAction, PrinterResult, PrinterService, PrinterSnapshot,
};
use focaldesk_settings::sound::SoundService;
use focaldesk_settings::theme_editor::{
    self, EditorDocument, FileAction, Projection, RuntimeStatus,
};
use focaldesk_settings::{
    bluetooth::{BluetoothAction, BluetoothService, BluetoothView},
    network::{NetworkAction, NetworkService, NetworkSnapshot},
};
use focaldesk_themes::{ThemeColor, ThemeColorSpace, ThemeDocument};
use serde::Serialize;
use std::sync::Mutex;
use tauri::Manager;

#[tauri::command]
fn get_exclusive_hdr() -> focaldesk_settings_core::ExclusiveHdrState {
    focaldesk_settings::displays::exclusive_hdr()
}
#[tauri::command]
async fn restart_exclusive_hdr(
    connector: String,
    expected: focaldesk_settings_core::ExclusiveHdrState,
    enabled: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::displays::restart_exclusive_hdr(connector, expected, enabled)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn start_speaker_test(
    service: tauri::State<'_, SoundService>,
    device_id: String,
) -> Result<(), String> {
    service.start_speaker(device_id)
}
#[tauri::command]
fn speaker_test_status(service: tauri::State<'_, SoundService>) -> Result<bool, String> {
    service.speaker_status()
}
#[tauri::command]
fn stop_speaker_test(service: tauri::State<'_, SoundService>) {
    service.stop_speaker();
}
#[tauri::command]
fn get_initial_panel() -> String {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == "--panel")
        .map(|pair| pair[1].to_lowercase())
        .unwrap_or_else(|| "appearance".into())
}
#[tauri::command]
fn open_project_resource(resource: String) -> Result<(), String> {
    let url = match resource.as_str() {
        "project" => "https://github.com/sjweiler/focaldesk",
        "license" => "https://github.com/sjweiler/focaldesk/blob/main/LICENSE",
        "credits" => "https://github.com/sjweiler/focaldesk#readme",
        _ => return Err("Unknown project resource.".into()),
    };
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_pointer_defaults() -> serde_json::Value {
    let input = focaldesk_settings_core::default_settings().input;
    serde_json::json!({ "pointer_speed": input.pointer_speed, "natural_scroll": input.natural_scroll,
        "workspace_swipe_fingers": input.workspace_swipe_fingers, "workspace_swipe_distance": input.workspace_swipe_distance })
}

#[tauri::command]
async fn get_saved_permissions(
) -> Result<focaldesk_settings::operations::PermissionSnapshot, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::operations::permissions)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn revoke_saved_permission(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::operations::revoke_permission(&id)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn clear_recent_activity() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::operations::clear_recent_activity)
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_power_status() -> Result<focaldesk_power::PowerSnapshot, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::operations::power_snapshot)
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn perform_power_action(
    action: focaldesk_settings::operations::PowerAction,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_settings::operations::power_action(action)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_display_icc() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose display ICC profile")
            .add_filter("ICC profiles", &["icc", "icm"])
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn get_hdr_presets() -> Vec<(&'static str, focaldesk_settings_core::HdrAppearance)> {
    focaldesk_settings::displays::hdr_presets()
}
#[tauri::command]
async fn set_display_trial_calibration_pattern(
    preview_id: u64,
    connector: String,
    pattern: focaldesk_settings_core::HdrCalibrationPattern,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_settings::displays::calibration_pattern(preview_id, connector, pattern)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_auto_hdr_xwayland_titles() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let value = focaldesk_ipc::send_desktop_get("displays.auto_hdr_xwayland_titles")?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn set_auto_hdr_xwayland_titles(titles: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_ipc::send_desktop_set(
            "displays.auto_hdr_xwayland_titles",
            serde_json::json!(titles),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_display_layout_preferences(
) -> Result<focaldesk_settings::displays::LayoutPreferences, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::displays::layout_preferences)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn apply_display_layout_preferences(
    expected: focaldesk_settings::displays::LayoutPreferences,
    requested: focaldesk_settings::displays::LayoutPreferences,
) -> Result<focaldesk_settings::displays::LayoutPreferences, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::displays::apply_layout(expected, requested)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_debug() -> Result<focaldesk_settings::debug::DebugSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| {
        focaldesk_settings::debug::snapshot().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_debug(
    change: focaldesk_settings::debug::DebugChange,
) -> Result<focaldesk_settings::debug::DebugResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::debug::apply(change).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn open_debug_log() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        focaldesk_settings::debug::open_log().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn copy_debug_diagnostics(app: tauri::AppHandle) -> Result<String, String> {
    let report = focaldesk_settings::debug::diagnostics();
    let (sender, receiver) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let result = gtk3::gdk::Display::default()
            .and_then(|display| gtk3::Clipboard::default(&display))
            .map(|clipboard| {
                clipboard.set_text(&report);
                "Diagnostics copied to clipboard.".to_string()
            })
            .ok_or_else(|| {
                "Clipboard is unavailable. Select and copy the diagnostic preview instead."
                    .to_string()
            });
        let _ = sender.send(result);
    })
    .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())??
}

#[tauri::command]
async fn get_workspaces() -> Result<focaldesk_settings::workspaces::WorkspaceSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| {
        focaldesk_settings::workspaces::snapshot().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_workspaces(
    change: focaldesk_settings::workspaces::WorkspaceChange,
) -> Result<focaldesk_settings::workspaces::WorkspaceResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::workspaces::apply(change).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn pick_workspace_wallpaper() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose workspace wallpaper")
            .add_filter("Wallpaper images", &["png", "jpg", "jpeg", "webp", "exr"])
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_chrome() -> Result<focaldesk_settings::chrome::ChromeSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| {
        focaldesk_settings::chrome::snapshot().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_chrome(
    change: focaldesk_settings::chrome::ChromeChange,
) -> Result<focaldesk_settings::chrome::ChromeResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        focaldesk_settings::chrome::apply(change).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_applications(
    service: tauri::State<'_, std::sync::Arc<ApplicationsService>>,
) -> Result<ApplicationsSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.snapshot().map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_applications(
    service: tauri::State<'_, std::sync::Arc<ApplicationsService>>,
    change: ApplicationsChange,
) -> Result<ApplicationsResult, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        service.apply(change).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_file_association(
    service: tauri::State<'_, std::sync::Arc<ApplicationsService>>,
    mime_type: String,
) -> Result<AssociationSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.association(mime_type).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn set_file_association(
    service: tauri::State<'_, std::sync::Arc<ApplicationsService>>,
    mime_type: String,
    expected: Option<String>,
    version: String,
    desktop_id: String,
) -> Result<AssociationResult, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .set_association(mime_type, expected, version, desktop_id)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_application_content_types(
    service: tauri::State<'_, std::sync::Arc<ApplicationsService>>,
) -> Result<Vec<String>, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.content_types())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_printers(
    service: tauri::State<'_, std::sync::Arc<PrinterService>>,
) -> Result<PrinterSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.snapshot())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_printer(
    service: tauri::State<'_, std::sync::Arc<PrinterService>>,
    action: PrinterAction,
) -> Result<PrinterResult, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.apply(action))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_print_jobs(
    service: tauri::State<'_, std::sync::Arc<PrinterService>>,
    name: String,
) -> Result<Vec<PrintJob>, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.jobs(name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_calendar(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
) -> Result<CalendarSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.snapshot())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn discover_calendars(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
    account_id: String,
) -> Result<CalendarPreferences, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.discover(account_id))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_calendar(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
    action: CalendarAction,
) -> Result<CalendarResult, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.apply(action))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn connect_calendar(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
    input: ConnectCalendar,
) -> Result<PublicFlow, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.connect(input))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn calendar_sign_in_status(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
    flow_id: String,
) -> Result<PublicFlow, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.poll(flow_id))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn open_calendar_browser(
    service: tauri::State<'_, std::sync::Arc<CalendarService>>,
    flow_id: String,
) -> Result<(), String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.open_browser(flow_id))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct EditorOpened {
    token: u64,
    editor: EditorDocument,
    runtime: Option<RuntimeStatus>,
    runtime_error: Option<String>,
}
#[derive(Default)]
struct EditorSession {
    token: Option<u64>,
    preview: bool,
}
#[derive(Default)]
struct EditorService(std::sync::Mutex<EditorSession>);
impl EditorService {
    fn close(&self, token: Option<u64>) {
        if let Ok(mut session) = self.0.lock() {
            if token.is_none() || token == session.token {
                if session.preview {
                    if let Some(token) = session.token {
                        let _ = theme_editor::runtime(Some(
                            focaldesk_ipc::ThemeEditorCommand::RevertOwned { token },
                        ));
                    }
                }
                session.token = None;
                session.preview = false;
            }
        }
    }
}
#[tauri::command]
async fn open_theme_editor(
    service: tauri::State<'_, std::sync::Arc<EditorService>>,
) -> Result<EditorOpened, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.close(None);
        let token = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
            % (1_u128 << 53)) as u64;
        let editor = theme_editor::default_document().map_err(|e| format!("{e:#}"))?;
        service.0.lock().map_err(|e| e.to_string())?.token = Some(token);
        let (runtime, runtime_error) = match theme_editor::runtime(None) {
            Ok(status) => (Some(status), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        Ok(EditorOpened {
            token,
            editor,
            runtime,
            runtime_error,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn theme_editor_file(action: FileAction) -> Result<EditorDocument, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|e| e.to_string())?;
        theme_editor::file_action(action).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn project_theme(document: ThemeDocument) -> Result<Projection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        theme_editor::project(&document).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn convert_theme_color(color: ThemeColor, space: ThemeColorSpace) -> Result<ThemeColor, String> {
    theme_editor::convert_color(color, space).map_err(|e| e.to_string())
}
#[tauri::command]
async fn theme_editor_runtime(
    service: tauri::State<'_, std::sync::Arc<EditorService>>,
    token: u64,
    command: String,
    document: Option<ThemeDocument>,
    expected_revision: Option<u64>,
) -> Result<RuntimeStatus, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = service.0.lock().map_err(|e| e.to_string())?;
        if session.token != Some(token) {
            return Err("This theme editor has closed. Reopen it before continuing.".into());
        }
        let command = match command.as_str() {
            "status" => None,
            "preview" => Some(focaldesk_ipc::ThemeEditorCommand::PreviewOwned {
                token,
                document: document.ok_or("Missing theme document")?,
            }),
            "apply" => Some(focaldesk_ipc::ThemeEditorCommand::ApplyOwned {
                token,
                document: document.ok_or("Missing theme document")?,
                expected_revision: expected_revision
                    .ok_or("Refresh the runtime status before applying")?,
            }),
            "revert" => Some(focaldesk_ipc::ThemeEditorCommand::RevertOwned { token }),
            _ => return Err("Unknown theme editor operation".into()),
        };
        if let Some(document) = match &command {
            Some(focaldesk_ipc::ThemeEditorCommand::PreviewOwned { document, .. })
            | Some(focaldesk_ipc::ThemeEditorCommand::ApplyOwned { document, .. }) => {
                Some(document)
            }
            _ => None,
        } {
            theme_editor::project(document).map_err(|e| format!("{e:#}"))?;
        }
        let status = theme_editor::runtime(command).map_err(|e| format!("{e:#}"))?;
        session.preview = status.preview_active;
        Ok(status)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn close_theme_editor(
    service: tauri::State<'_, std::sync::Arc<EditorService>>,
    token: u64,
) -> Result<(), String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.close(Some(token)))
        .await
        .map_err(|e| e.to_string())
}
#[derive(Serialize)]
struct ChosenThemeFile {
    path: String,
    hash: Option<String>,
}
#[tauri::command]
async fn choose_theme_file(kind: String) -> Result<Option<ChosenThemeFile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = rfd::FileDialog::new();
        let path = match kind.as_str() {
            "open" => dialog
                .add_filter("FocalDesk theme", &["toml", "fdtheme"])
                .pick_file(),
            "save" => dialog
                .add_filter("Theme document", &["toml"])
                .set_file_name("theme.toml")
                .save_file(),
            "export" => dialog
                .add_filter("Theme package", &["fdtheme"])
                .set_file_name("theme.fdtheme")
                .save_file(),
            "wallpaper" => dialog
                .add_filter("Wallpaper image", &["png", "jpg", "jpeg", "webp"])
                .pick_file(),
            _ => return Err("Unknown file selection".into()),
        };
        path.map(|path| {
            let hash = if kind == "wallpaper" || !path.exists() {
                None
            } else {
                Some(theme_editor::file_hash(&path).map_err(|e| e.to_string())?)
            };
            Ok(ChosenThemeFile {
                path: path.to_string_lossy().into_owned(),
                hash,
            })
        })
        .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_network(
    service: tauri::State<'_, std::sync::Arc<NetworkService>>,
) -> Result<NetworkSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.snapshot())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_network(
    service: tauri::State<'_, std::sync::Arc<NetworkService>>,
    action: NetworkAction,
) -> Result<NetworkSnapshot, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.apply(action))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn open_bluetooth(
    service: tauri::State<'_, std::sync::Arc<BluetoothService>>,
) -> Result<BluetoothView, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.open())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_bluetooth(
    service: tauri::State<'_, std::sync::Arc<BluetoothService>>,
    lease: u64,
) -> Result<BluetoothView, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.snapshot(lease))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn apply_bluetooth(
    service: tauri::State<'_, std::sync::Arc<BluetoothService>>,
    lease: u64,
    action: BluetoothAction,
) -> Result<BluetoothView, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.apply(lease, action))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn close_bluetooth(
    service: tauri::State<'_, std::sync::Arc<BluetoothService>>,
    lease: u64,
) -> Result<(), String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.close(lease))
        .await
        .map_err(|e| e.to_string())
}

static APPLY_LOCK: Mutex<()> = Mutex::new(());

#[tauri::command]
async fn get_accessibility(
) -> Result<focaldesk_settings::accessibility::AccessibilitySnapshot, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::accessibility::snapshot)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn apply_accessibility(
    expected: focaldesk_settings::accessibility::AccessibilityDraft,
    requested: focaldesk_settings::accessibility::AccessibilityDraft,
) -> Result<focaldesk_settings::accessibility::AccessibilityResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|error| error.to_string())?;
        focaldesk_settings::accessibility::apply(expected, requested)
            .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_preferences() -> Result<focaldesk_settings::preferences::Preferences, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::preferences::snapshot)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn apply_preferences(
    change: focaldesk_settings::preferences::PreferenceChange,
) -> Result<focaldesk_settings::preferences::ApplyResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|error| error.to_string())?;
        focaldesk_settings::preferences::apply(change).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_appearance() -> Result<AppearanceSnapshot, String> {
    tauri::async_runtime::spawn_blocking(appearance::snapshot)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn apply_appearance(
    expected: Appearance,
    requested: Appearance,
) -> Result<AppearanceSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY_LOCK.lock().map_err(|error| error.to_string())?;
        appearance::apply(expected, requested).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_displays() -> Result<focaldesk_ipc::DisplayConfiguration, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::displays::snapshot)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn preview_displays(
    expected: Vec<focaldesk_settings_core::OutputConfig>,
    outputs: Vec<focaldesk_settings_core::OutputConfig>,
) -> Result<focaldesk_ipc::DisplayConfiguration, String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_settings::displays::preview(expected, outputs)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn confirm_displays(preview_id: u64) -> Result<focaldesk_ipc::DisplayConfiguration, String> {
    tauri::async_runtime::spawn_blocking(move || focaldesk_settings::displays::confirm(preview_id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn revert_displays(preview_id: u64) -> Result<focaldesk_ipc::DisplayConfiguration, String> {
    tauri::async_runtime::spawn_blocking(move || focaldesk_settings::displays::revert(preview_id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_audio(
    app: tauri::AppHandle,
) -> Result<focaldesk_audio::controls::AudioSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<SoundService>().snapshot())
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn apply_audio(
    app: tauri::AppHandle,
    action: focaldesk_audio::controls::AudioAction,
) -> Result<focaldesk_audio::controls::AudioSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<SoundService>().apply(action))
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn start_input_test(
    app: tauri::AppHandle,
    device_id: String,
) -> Result<focaldesk_audio::meter::MeterSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<SoundService>().start_meter(device_id))
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn get_input_level(
    app: tauri::AppHandle,
    id: u64,
) -> Result<focaldesk_audio::meter::MeterSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<SoundService>().meter_status(id))
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn stop_input_test(app: tauri::AppHandle, id: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<SoundService>().stop_meter(id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_input_configuration() -> Result<focaldesk_ipc::InputConfiguration, String> {
    tauri::async_runtime::spawn_blocking(focaldesk_settings::input::snapshot)
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn apply_input_configuration(
    expected: focaldesk_settings_core::InputSettings,
    expected_saved: focaldesk_settings_core::InputSettings,
    input: focaldesk_settings_core::InputSettings,
) -> Result<focaldesk_ipc::InputConfiguration, String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_settings::input::apply(expected, expected_saved, input)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(Serialize)]
struct About {
    version: &'static str,
    operating_system: String,
    architecture: &'static str,
    session: &'static str,
}

#[tauri::command]
fn get_about() -> About {
    let operating_system = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|source| {
            source.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|name| name.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    About {
        version: env!("CARGO_PKG_VERSION"),
        operating_system,
        architecture: std::env::consts::ARCH,
        session: if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "Wayland"
        } else {
            "Desktop session"
        },
    }
}

#[tauri::command]
fn open_existing_settings(panel: String) -> Result<(), String> {
    let panel = focaldesk_settings::navigation::existing_panel(&panel)?;
    let executable = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("focaldesk-settings");
    std::process::Command::new(executable)
        .args(["--panel", panel])
        .spawn()
        .map_err(|error| format!("Could not open existing Settings: {error}"))?;
    Ok(())
}

fn main() {
    focaldesk_logging::init_default_logging();
    tauri::Builder::default()
        .manage(SoundService::default())
        .manage(std::sync::Arc::new(NetworkService::default()))
        .manage(std::sync::Arc::new(BluetoothService::default()))
        .manage(std::sync::Arc::new(EditorService::default()))
        .manage(std::sync::Arc::new(CalendarService::default()))
        .manage(std::sync::Arc::new(PrinterService::default()))
        .manage(std::sync::Arc::new(ApplicationsService::default()))
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Focused(focused) => {
                window.state::<SoundService>().set_focused(*focused)
            }
            tauri::WindowEvent::Destroyed | tauri::WindowEvent::CloseRequested { .. } => {
                let editor = window
                    .state::<std::sync::Arc<EditorService>>()
                    .inner()
                    .clone();
                std::thread::spawn(move || editor.close(None));
                let bluetooth = window
                    .state::<std::sync::Arc<BluetoothService>>()
                    .inner()
                    .clone();
                std::thread::spawn(move || bluetooth.stop_all());
                window.state::<SoundService>().set_focused(false)
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_exclusive_hdr,
            restart_exclusive_hdr,
            start_speaker_test,
            speaker_test_status,
            stop_speaker_test,
            get_initial_panel,
            open_project_resource,
            get_pointer_defaults,
            get_saved_permissions,
            revoke_saved_permission,
            clear_recent_activity,
            get_power_status,
            perform_power_action,
            get_appearance,
            pick_display_icc,
            get_hdr_presets,
            set_display_trial_calibration_pattern,
            get_display_layout_preferences,
            apply_display_layout_preferences,
            get_auto_hdr_xwayland_titles,
            set_auto_hdr_xwayland_titles,
            get_chrome,
            apply_chrome,
            get_workspaces,
            apply_workspaces,
            pick_workspace_wallpaper,
            get_debug,
            apply_debug,
            open_debug_log,
            copy_debug_diagnostics,
            get_applications,
            apply_applications,
            get_file_association,
            set_file_association,
            get_application_content_types,
            get_printers,
            apply_printer,
            get_print_jobs,
            get_calendar,
            discover_calendars,
            apply_calendar,
            connect_calendar,
            calendar_sign_in_status,
            open_calendar_browser,
            open_theme_editor,
            close_theme_editor,
            theme_editor_file,
            theme_editor_runtime,
            project_theme,
            convert_theme_color,
            choose_theme_file,
            get_network,
            apply_network,
            open_bluetooth,
            get_bluetooth,
            apply_bluetooth,
            close_bluetooth,
            apply_appearance,
            get_displays,
            preview_displays,
            confirm_displays,
            revert_displays,
            get_audio,
            apply_audio,
            start_input_test,
            get_input_level,
            stop_input_test,
            get_input_configuration,
            apply_input_configuration,
            get_about,
            get_preferences,
            get_accessibility,
            apply_accessibility,
            apply_preferences,
            open_existing_settings,
        ])
        .run(tauri::generate_context!())
        .expect("run FocalDesk Settings preview");
}
