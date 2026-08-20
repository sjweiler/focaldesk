use crate::desktop_frame::DesktopFrameCtx;
use crate::types::{SettingKey, SystemCommand, UiAction};
use chrono::{Datelike, Local, NaiveDate};
use focaldesk_ipc::{
    NotificationIpcRequest, NotificationIpcResponse, UpdateIpcRequest, UpdateIpcResponse,
    send_notification_request, send_update_request,
};
use focaldesk_updates::UpdateSnapshot;
use std::collections::HashSet;
pub mod settings;

pub use settings::SettingsPanel;

// egui_panel.rs
pub trait EguiPanelView {
    fn title(&self) -> &'static str;
    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    );
}

//#[derive(Default)]
//pub struct SettingsPanel {
//    pub open: bool,
//}

#[derive(Default)]
pub struct DebugPanel {
    pub open: bool,
}

#[derive(Default)]
pub struct PowerPanel {
    pub open: bool,
}

pub struct AudioPanel {
    pub open: bool,
    volume: f32,
}

pub struct NetworkPanel {
    pub open: bool,
    wifi_enabled: bool,
}

pub struct BluetoothPanel {
    pub open: bool,
    bluetooth_enabled: bool,
}

pub struct NotificationHistoryPanel {
    pub open: bool,
    entries: Vec<focaldesk_notifications::NotificationSnapshot>,
    do_not_disturb: bool,
    marked_read: bool,
    last_poll: std::time::Instant,
}

impl Default for NotificationHistoryPanel {
    fn default() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            do_not_disturb: false,
            marked_read: false,
            last_poll: std::time::Instant::now() - std::time::Duration::from_secs(10),
        }
    }
}

impl EguiPanelView for NotificationHistoryPanel {
    fn title(&self) -> &'static str {
        "Notifications"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            self.marked_read = false;
            return;
        }
        if !self.marked_read {
            let _ = send_notification_request(&NotificationIpcRequest::MarkAllRead);
            self.marked_read = true;
        }
        if frame_ctx.now.saturating_duration_since(self.last_poll)
            >= std::time::Duration::from_millis(500)
        {
            self.last_poll = frame_ctx.now;
            if let Ok(NotificationIpcResponse::History { notifications }) =
                send_notification_request(&NotificationIpcRequest::GetHistory)
            {
                self.entries = notifications;
            }
            if let Ok(NotificationIpcResponse::State { do_not_disturb }) =
                send_notification_request(&NotificationIpcRequest::GetState)
            {
                self.do_not_disturb = do_not_disturb;
            }
        }
        let mut open = self.open;
        egui::Window::new(self.title())
            .default_pos(egui::pos2(
                (frame_ctx.work.loc.x + frame_ctx.work.size.w - 360) as f32,
                (frame_ctx.work.loc.y + 24) as f32,
            ))
            .default_width(340.0)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Notifications");
                    let mut dnd = self.do_not_disturb;
                    if ui.checkbox(&mut dnd, "DND").changed() {
                        let _ =
                            send_notification_request(&NotificationIpcRequest::SetDoNotDisturb {
                                enabled: dnd,
                            });
                        self.do_not_disturb = dnd;
                    }
                    if ui.button("Clear all").clicked() {
                        let _ = send_notification_request(&NotificationIpcRequest::ClearHistory);
                        self.entries.clear();
                    }
                });
                ui.separator();
                if self.entries.is_empty() {
                    ui.label("No notifications");
                } else {
                    for entry in &self.entries {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(&entry.title);
                                if ui.small_button("Dismiss").clicked() {
                                    let _ = send_notification_request(
                                        &NotificationIpcRequest::Dismiss { id: entry.id },
                                    );
                                }
                            });
                            ui.label(&entry.body);
                        });
                    }
                }
            });
        self.open = open;
    }
}

pub struct UpdatesPanel {
    pub open: bool,
    snapshot: UpdateSnapshot,
    selected: HashSet<String>,
    last_poll: std::time::Instant,
    requested_refresh: bool,
}

impl Default for UpdatesPanel {
    fn default() -> Self {
        Self {
            open: false,
            snapshot: UpdateSnapshot::default(),
            selected: HashSet::new(),
            last_poll: std::time::Instant::now() - std::time::Duration::from_secs(10),
            requested_refresh: false,
        }
    }
}

impl EguiPanelView for UpdatesPanel {
    fn title(&self) -> &'static str {
        "Updates"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            self.requested_refresh = false;
            return;
        }
        if !self.requested_refresh {
            let _ = send_update_request(&UpdateIpcRequest::Refresh {
                refresh_metadata: false,
            });
            self.requested_refresh = true;
        }
        let poll_every = if self.snapshot.checking || self.snapshot.installing {
            std::time::Duration::from_millis(400)
        } else {
            std::time::Duration::from_millis(800)
        };
        if frame_ctx.now.saturating_duration_since(self.last_poll) >= poll_every {
            self.last_poll = frame_ctx.now;
            if let Ok(UpdateIpcResponse::State { snapshot }) =
                send_update_request(&UpdateIpcRequest::GetState)
            {
                let ids: HashSet<String> = snapshot
                    .packages
                    .iter()
                    .map(|package| package.id.clone())
                    .collect();
                let previous_ids: HashSet<String> = self
                    .snapshot
                    .packages
                    .iter()
                    .map(|package| package.id.clone())
                    .collect();
                if ids != previous_ids {
                    self.selected.retain(|id| ids.contains(id));
                    if self.selected.is_empty() {
                        self.selected = ids;
                    }
                }
                self.snapshot = snapshot;
            }
        }

        let mut open = self.open;
        let mut install_ids: Option<Vec<String>> = None;
        let mut install_all = false;
        let mut refresh_metadata = false;
        let mut select_all = false;
        let mut select_none = false;
        let busy = self.snapshot.checking || self.snapshot.installing;
        let available = self.snapshot.packages.len();
        let selected_count = self.selected.len();

        egui::Window::new(self.title())
            .default_pos(egui::pos2(
                (frame_ctx.work.loc.x + frame_ctx.work.size.w - 420) as f32,
                (frame_ctx.work.loc.y + 24) as f32,
            ))
            .default_width(400.0)
            .default_height(480.0)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("System updates");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if ui.button("Refresh").clicked() {
                                refresh_metadata = true;
                            }
                        });
                    });
                });
                if let Some(progress) = &self.snapshot.progress {
                    ui.label(progress);
                } else if available == 0 {
                    ui.label("No updates available.");
                } else {
                    ui.label(format!("{available} update(s) available"));
                }
                if let Some(error) = &self.snapshot.last_error {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy && available > 0, |ui| {
                        if ui.button("Select all").clicked() {
                            select_all = true;
                        }
                        if ui.button("Select none").clicked() {
                            select_none = true;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        if self.snapshot.packages.is_empty() && !busy {
                            ui.label("Your system is up to date.");
                        }
                        for package in &self.snapshot.packages {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    let mut checked = self.selected.contains(&package.id);
                                    if ui
                                        .add_enabled(!busy, egui::Checkbox::new(&mut checked, ""))
                                        .changed()
                                    {
                                        if checked {
                                            self.selected.insert(package.id.clone());
                                        } else {
                                            self.selected.remove(&package.id);
                                        }
                                    }
                                    ui.vertical(|ui| {
                                        ui.strong(package.display_title());
                                        let meta = [package.arch.as_str(), package.repo.as_str()]
                                            .into_iter()
                                            .filter(|part| !part.is_empty())
                                            .collect::<Vec<_>>()
                                            .join(" · ");
                                        if !meta.is_empty() {
                                            ui.weak(meta);
                                        }
                                        if let Some(detail) = package.detail_text() {
                                            ui.label(detail);
                                        }
                                    });
                                });
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy && selected_count > 0, |ui| {
                        if ui
                            .button(format!("Install selected ({selected_count})"))
                            .clicked()
                        {
                            install_ids = Some(self.selected.iter().cloned().collect());
                        }
                    });
                    ui.add_enabled_ui(!busy && available > 0, |ui| {
                        if ui.button("Install all").clicked() {
                            install_all = true;
                        }
                    });
                });
            });

        if select_all {
            self.selected = self
                .snapshot
                .packages
                .iter()
                .map(|package| package.id.clone())
                .collect();
        }
        if select_none {
            self.selected.clear();
        }
        if refresh_metadata {
            let _ = send_update_request(&UpdateIpcRequest::Refresh {
                refresh_metadata: true,
            });
        }
        if let Some(ids) = install_ids {
            let _ = send_update_request(&UpdateIpcRequest::Install { ids });
        }
        if install_all {
            let _ = send_update_request(&UpdateIpcRequest::InstallAll);
        }
        self.open = open;
    }
}

#[derive(Default)]
pub struct CalendarPanel {
    pub open: bool,
}

/// UI-facing snapshot of a clipboard-history entry; the engine owns the real store.
#[derive(Debug, Clone)]
pub struct ClipboardEntryView {
    pub id: u64,
    pub preview: String,
}

#[derive(Default)]
pub struct ClipboardPanel {
    pub open: bool,
    pub entries: Vec<ClipboardEntryView>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceEntryView {
    pub number: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Default)]
pub struct WorkspacesPanel {
    pub open: bool,
    pub entries: Vec<WorkspaceEntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDialogMode {
    Add,
    Delete,
}

pub struct WorkspaceDialog {
    pub open: bool,
    pub mode: WorkspaceDialogMode,
    pub name: String,
}

impl Default for WorkspaceDialog {
    fn default() -> Self {
        Self {
            open: false,
            mode: WorkspaceDialogMode::Add,
            name: String::new(),
        }
    }
}

impl Default for AudioPanel {
    fn default() -> Self {
        Self {
            open: false,
            volume: 0.5,
        }
    }
}

impl WorkspaceDialog {
    pub fn open_add(&mut self, name: impl Into<String>) {
        self.open = true;
        self.mode = WorkspaceDialogMode::Add;
        self.name = name.into();
    }

    pub fn open_delete(&mut self) {
        self.open = true;
        self.mode = WorkspaceDialogMode::Delete;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let mut open = self.open;
        let mut close_requested = false;
        let title = match self.mode {
            WorkspaceDialogMode::Add => "Add Workspace",
            WorkspaceDialogMode::Delete => "Delete Workspace",
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 32.0,
                frame_ctx.work.loc.y as f32 + 32.0,
            ))
            .open(&mut open)
            .show(ctx, |ui| {
                match self.mode {
                    WorkspaceDialogMode::Add => {
                        ui.label("Workspace name");
                        ui.text_edit_singleline(&mut self.name);
                    }
                    WorkspaceDialogMode::Delete => {
                        ui.label("Delete the current workspace?");
                    }
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("No").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Yes").clicked() {
                        match self.mode {
                            WorkspaceDialogMode::Add => {
                                actions
                                    .push(UiAction::CreateWorkspace(self.name.trim().to_string()));
                            }
                            WorkspaceDialogMode::Delete => {
                                actions.push(UiAction::DeleteWorkspace);
                            }
                        }
                        close_requested = true;
                    }
                });
            });

        self.open = open && !close_requested;
    }
}

impl WorkspacesPanel {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let mut open = self.open;
        let mut selected = None;
        egui::Window::new("Workspaces")
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Choose a workspace for this display");
                ui.add_space(8.0);
                for entry in &self.entries {
                    let label = format!("{}  {}", entry.number, entry.name);
                    if ui.selectable_label(entry.active, label).clicked() {
                        selected = Some(entry.number);
                    }
                }
            });

        if let Some(workspace) = selected {
            actions.push(UiAction::FocusWorkspace(workspace));
            open = false;
        }
        self.open = open;
    }
}

impl Default for NetworkPanel {
    fn default() -> Self {
        Self {
            open: false,
            wifi_enabled: true,
        }
    }
}

impl Default for BluetoothPanel {
    fn default() -> Self {
        Self {
            open: false,
            bluetooth_enabled: true,
        }
    }
}

/*impl EguiPanelView for SettingsPanel {
    fn title(&self) -> &'static str {
        "Settings"
    }
    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        egui::Window::new("Settings")
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .default_width(520.0)
            .open(&mut self.open)
            .show(ctx, |ui| {
                ui.heading("FocalDesk Settings");
                ui.label(format!("Output: {:?}", frame_ctx.rendering_output));
            });
    }
}
*/

impl EguiPanelView for PowerPanel {
    fn title(&self) -> &'static str {
        "Power"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 220.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Power Menu")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Power");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("X").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(4.0);

                if power_button(ui, "Lock").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Lock));
                    close_requested = true;
                }
                if power_button(ui, "Suspend").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Suspend));
                    close_requested = true;
                }
                if power_button(ui, "Hibernate").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Hibernate));
                    close_requested = true;
                }
                if power_button(ui, "Logout").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Logout));
                    close_requested = true;
                }
                if power_button(ui, "Restart").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Restart));
                    close_requested = true;
                }
                if power_button(ui, "Shutdown").clicked() {
                    actions.push(UiAction::SystemCommand(SystemCommand::Shutdown));
                    close_requested = true;
                }
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

impl EguiPanelView for AudioPanel {
    fn title(&self) -> &'static str {
        "Audio"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 260.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Audio")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Audio");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("X").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.label("Volume");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("{:.0}%", self.volume * 100.0));
                    });
                });

                let changed = ui
                    .add(
                        egui::Slider::new(&mut self.volume, 0.0..=1.0)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always),
                    )
                    .changed();

                if changed {
                    actions.push(UiAction::SetVolume(self.volume));
                }
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

impl EguiPanelView for ClipboardPanel {
    fn title(&self) -> &'static str {
        "Clipboard"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 320.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Clipboard")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Clipboard");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("X").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                ui.add_space(4.0);

                if self.entries.is_empty() {
                    ui.label("No clipboard history yet.");
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for entry in &self.entries {
                            let preview: String = entry.preview.chars().take(120).collect();
                            if ui.button(preview).clicked() {
                                actions.push(UiAction::SelectClipboardEntry(entry.id));
                                close_requested = true;
                            }
                        }
                    });
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

impl EguiPanelView for NetworkPanel {
    fn title(&self) -> &'static str {
        "Network"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 280.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Network")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                panel_header(ui, "Network", &mut close_requested);
                ui.separator();
                ui.add_space(4.0);

                if ui
                    .checkbox(&mut self.wifi_enabled, "Wifi")
                    .on_hover_text("Enable or disable the default NetworkManager wifi radio")
                    .changed()
                {
                    actions.push(UiAction::SetSetting(SettingKey::Wifi, self.wifi_enabled));
                }

                ui.add_space(6.0);
                ui.label(if self.wifi_enabled {
                    "Wifi radio is enabled."
                } else {
                    "Wifi radio is disabled."
                });
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

impl EguiPanelView for BluetoothPanel {
    fn title(&self) -> &'static str {
        "Bluetooth"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 280.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Bluetooth")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                panel_header(ui, "Bluetooth", &mut close_requested);
                ui.separator();
                ui.add_space(4.0);

                if ui
                    .checkbox(&mut self.bluetooth_enabled, "Bluetooth")
                    .on_hover_text("Enable or disable the default bluetooth controller")
                    .changed()
                {
                    actions.push(UiAction::SetSetting(
                        SettingKey::Bluetooth,
                        self.bluetooth_enabled,
                    ));
                }

                ui.add_space(6.0);
                ui.label(if self.bluetooth_enabled {
                    "Bluetooth controller is powered on."
                } else {
                    "Bluetooth controller is powered off."
                });
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

impl EguiPanelView for CalendarPanel {
    fn title(&self) -> &'static str {
        "Calendar"
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let panel_width = 310.0;
        let x = (frame_ctx.work.loc.x + frame_ctx.work.size.w) as f32 - panel_width - 24.0;
        let y = frame_ctx.work.loc.y as f32 + 24.0;
        let mut open = self.open;
        let mut close_requested = false;

        let response = egui::Window::new("Calendar")
            .default_pos(egui::pos2(x.max(16.0), y.max(16.0)))
            .default_width(panel_width)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                panel_header(ui, "Calendar", &mut close_requested);
                ui.separator();
                ui.add_space(4.0);
                draw_calendar_month(ui);
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

fn draw_calendar_month(ui: &mut egui::Ui) {
    let today = Local::now().date_naive();
    let Some(first_day) = NaiveDate::from_ymd_opt(today.year(), today.month(), 1) else {
        return;
    };
    let first_weekday = first_day.weekday().num_days_from_sunday() as usize;
    let days_in_month = days_in_month(today.year(), today.month());

    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(today.format("%B %Y").to_string())
                .size(18.0)
                .strong(),
        );
    });
    ui.add_space(8.0);

    egui::Grid::new("focaldesk_calendar_month")
        .num_columns(7)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            for day in ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] {
                ui.label(egui::RichText::new(day).weak().size(12.0));
            }
            ui.end_row();

            let mut day = 1u32;
            for week in 0..6 {
                for weekday in 0..7 {
                    if (week == 0 && weekday < first_weekday) || day > days_in_month {
                        ui.label("");
                        continue;
                    }

                    let is_today = day == today.day();
                    let text = egui::RichText::new(day.to_string()).size(14.0);
                    if is_today {
                        ui.add_sized(
                            [30.0, 26.0],
                            egui::Button::new(text.color(egui::Color32::WHITE))
                                .fill(egui::Color32::from_rgb(28, 115, 190))
                                .corner_radius(egui::CornerRadius::same(8)),
                        );
                    } else {
                        ui.add_sized([30.0, 26.0], egui::Label::new(text));
                    }
                    day += 1;
                }
                ui.end_row();

                if day > days_in_month {
                    break;
                }
            }
        });
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let Some(next_first) = NaiveDate::from_ymd_opt(next_year, next_month, 1) else {
        return 31;
    };
    next_first.pred_opt().map(|date| date.day()).unwrap_or(31)
}

fn panel_header(ui: &mut egui::Ui, title: &str, close_requested: &mut bool) {
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("X").clicked() {
                *close_requested = true;
            }
        });
    });
}

fn power_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized(
        [190.0, 36.0],
        egui::Button::new(egui::RichText::new(label).size(15.0))
            .corner_radius(egui::CornerRadius::same(8))
            .frame(true),
    )
}

impl EguiPanelView for DebugPanel {
    fn title(&self) -> &'static str {
        "Debug"
    }
    fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        egui::Window::new("Debug")
            .open(&mut self.open)
            .show(ctx, |ui| {
                ui.heading("Debug");
                ui.label(format!("Work area: {:?}", frame_ctx.work));
            });
    }
}
