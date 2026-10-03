use crate::desktop_frame::DesktopFrameCtx;
use crate::types::{SettingKey, SplitLayoutPreset, SystemCommand, UiAction};
use chrono::{Datelike, Local, NaiveDate};
use focaldesk_ipc::{
    NotificationIpcRequest, NotificationIpcResponse, UpdateIpcRequest, UpdateIpcResponse,
    send_notification_request, send_update_request,
};
use focaldesk_types::WindowId;
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
            .fade_in(false)
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
            .fade_in(false)
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
    pub windows: Vec<WorkspaceWindowPreview>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceWindowPreview {
    pub title: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Default)]
pub struct WorkspacesPanel {
    pub open: bool,
    pub entries: Vec<WorkspaceEntryView>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SplitLayoutAvailability {
    pub focused_window: bool,
    pub side_by_side: bool,
    pub thirds: bool,
    pub stacked: bool,
    pub quadrants: bool,
}

#[derive(Default)]
pub struct SplitLayoutPanel {
    pub open: bool,
    pub availability: SplitLayoutAvailability,
    keyboard_preset: Option<SplitLayoutPreset>,
}

#[derive(Debug, Clone)]
pub struct SplitAssistEntryView {
    pub id: WindowId,
    pub title: String,
    pub app_name: String,
}

#[derive(Default)]
pub struct SplitAssistPanel {
    pub open: bool,
    pub entries: Vec<SplitAssistEntryView>,
    pub(crate) keyboard_index: usize,
}

#[derive(Default)]
pub struct SplitGroupPanel {
    pub open: bool,
    pub anchor: Option<egui::Pos2>,
}

impl EguiPanelView for SplitGroupPanel {
    fn title(&self) -> &'static str {
        "Split Group"
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
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }
        let mut open = self.open;
        let mut action = None;
        egui::Window::new(self.title())
            .fade_in(false)
            .collapsible(false)
            .resizable(false)
            .default_pos(self.anchor.unwrap_or_else(|| {
                egui::pos2(
                    (frame_ctx.work.loc.x + frame_ctx.work.size.w / 2 - 110) as f32,
                    (frame_ctx.work.loc.y + frame_ctx.work.size.h / 2 - 80) as f32,
                )
            }))
            .default_width(220.0)
            .open(&mut open)
            .show(ctx, |ui| {
                if ui.button("Swap panes").clicked() {
                    action = Some(UiAction::SwapSplitPanes);
                }
                if ui.button("Replace focused pane").clicked() {
                    action = Some(UiAction::ReplaceSplitWindow);
                }
                if ui.button("Exit split").clicked() {
                    action = Some(UiAction::ExitSplitGroup);
                }
            });
        if let Some(action) = action {
            actions.push(action);
            open = false;
        }
        self.open = open;
    }
}

impl EguiPanelView for SplitAssistPanel {
    fn title(&self) -> &'static str {
        "Split Assist"
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
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            actions.push(UiAction::CancelSplitAssist);
            return;
        }

        if self.entries.is_empty() {
            self.keyboard_index = 0;
        } else {
            self.keyboard_index = self.keyboard_index.min(self.entries.len() - 1);
            let forward = ctx.input(|input| {
                input.key_pressed(egui::Key::ArrowDown) || input.key_pressed(egui::Key::ArrowRight)
            });
            let backward = ctx.input(|input| {
                input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowLeft)
            });
            if forward {
                self.keyboard_index = move_selection(self.keyboard_index, self.entries.len(), true);
            } else if backward {
                self.keyboard_index =
                    move_selection(self.keyboard_index, self.entries.len(), false);
            }
            if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                let id = self.entries[self.keyboard_index].id;
                actions.push(UiAction::SelectSplitAssistWindow(id));
                self.open = false;
                return;
            }
        }

        let mut open = self.open;
        let mut selected = None;
        egui::Window::new(self.title())
            .fade_in(false)
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(
                (frame_ctx.work.loc.x + frame_ctx.work.size.w / 2 - 240) as f32,
                (frame_ctx.work.loc.y + 24) as f32,
            ))
            .default_width(480.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading("Choose a window for the remaining pane");
                ui.label("Use arrow keys and Enter, or click. Esc leaves the pane empty.");
                ui.separator();

                for (index, entry) in self.entries.iter().enumerate() {
                    let desired = egui::vec2(ui.available_width(), 66.0);
                    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
                    if response.hovered() {
                        self.keyboard_index = index;
                    }
                    let active = self.keyboard_index == index;
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::Button,
                            true,
                            active,
                            format!("{} — {}", entry.title, entry.app_name),
                        )
                    });
                    if active {
                        response.request_focus();
                    }
                    ui.painter().rect_filled(
                        rect,
                        7.0,
                        if active {
                            egui::Color32::from_rgb(43, 65, 87)
                        } else {
                            egui::Color32::from_rgb(28, 34, 46)
                        },
                    );
                    ui.painter().rect_stroke(
                        rect,
                        7.0,
                        egui::Stroke::new(
                            1.0,
                            if active {
                                egui::Color32::from_rgb(84, 188, 255)
                            } else {
                                egui::Color32::from_gray(76)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    let icon = egui::Rect::from_min_size(
                        rect.min + egui::vec2(10.0, 10.0),
                        egui::vec2(46.0, 46.0),
                    );
                    ui.painter()
                        .rect_filled(icon, 5.0, egui::Color32::from_rgb(51, 116, 166));
                    let initial = entry
                        .app_name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string();
                    ui.painter().text(
                        icon.center(),
                        egui::Align2::CENTER_CENTER,
                        initial,
                        egui::FontId::proportional(22.0),
                        egui::Color32::WHITE,
                    );
                    ui.painter().text(
                        rect.min + egui::vec2(68.0, 17.0),
                        egui::Align2::LEFT_TOP,
                        &entry.title,
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                    );
                    ui.painter().text(
                        rect.min + egui::vec2(68.0, 41.0),
                        egui::Align2::LEFT_TOP,
                        &entry.app_name,
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_gray(170),
                    );
                    if response.clicked() {
                        selected = Some(entry.id);
                    }
                }
            });

        if let Some(id) = selected {
            actions.push(UiAction::SelectSplitAssistWindow(id));
            open = false;
        } else if !open {
            actions.push(UiAction::CancelSplitAssist);
        }
        self.open = open;
    }
}

impl EguiPanelView for SplitLayoutPanel {
    fn title(&self) -> &'static str {
        "Split Layout"
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

        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }

        let available_presets = split_available_presets(self.availability);
        if !available_presets.contains(&self.keyboard_preset.unwrap_or(SplitLayoutPreset::LeftHalf))
        {
            self.keyboard_preset = available_presets.first().copied();
        }
        if let Some(current) = self.keyboard_preset {
            let current_index = available_presets
                .iter()
                .position(|preset| *preset == current)
                .unwrap_or(0);
            let forward = ctx.input(|input| {
                input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::ArrowDown)
            });
            let backward = ctx.input(|input| {
                input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::ArrowUp)
            });
            if forward || backward {
                let index = move_selection(current_index, available_presets.len(), forward);
                self.keyboard_preset = Some(available_presets[index]);
            }
            if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                actions.push(UiAction::ApplySplitLayout(self.keyboard_preset.unwrap()));
                self.open = false;
                return;
            }
        }

        let mut open = self.open;
        let mut selected = None;
        egui::Window::new(self.title())
            .fade_in(false)
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(
                (frame_ctx.work.loc.x + frame_ctx.work.size.w / 2 - 220) as f32,
                (frame_ctx.work.loc.y + 24) as f32,
            ))
            .default_width(440.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading("Place focused window");
                ui.label("Use arrow keys and Enter, or hover and click to apply.");
                ui.separator();

                if !self.availability.focused_window {
                    ui.label("Focus an application window to choose a split layout.");
                    return;
                }

                ui.label("Side by side");
                ui.horizontal(|ui| {
                    split_preset_button(
                        ui,
                        "Left ½",
                        SplitLayoutPreset::LeftHalf,
                        self.availability.side_by_side,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                    split_preset_button(
                        ui,
                        "Right ½",
                        SplitLayoutPreset::RightHalf,
                        self.availability.side_by_side,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                    split_preset_button(
                        ui,
                        "Left ⅔",
                        SplitLayoutPreset::LeftTwoThirds,
                        self.availability.thirds,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                    split_preset_button(
                        ui,
                        "Right ⅓",
                        SplitLayoutPreset::RightThird,
                        self.availability.thirds,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                });
                ui.horizontal(|ui| {
                    split_preset_button(
                        ui,
                        "Left ⅓",
                        SplitLayoutPreset::LeftThird,
                        self.availability.thirds,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                    split_preset_button(
                        ui,
                        "Right ⅔",
                        SplitLayoutPreset::RightTwoThirds,
                        self.availability.thirds,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                });

                ui.separator();
                ui.label("Stacked");
                ui.horizontal(|ui| {
                    split_preset_button(
                        ui,
                        "Top ½",
                        SplitLayoutPreset::TopHalf,
                        self.availability.stacked,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                    split_preset_button(
                        ui,
                        "Bottom ½",
                        SplitLayoutPreset::BottomHalf,
                        self.availability.stacked,
                        &mut self.keyboard_preset,
                        &mut selected,
                    );
                });

                ui.separator();
                ui.label("Quadrants");
                ui.horizontal(|ui| {
                    for (label, preset) in [
                        ("Top left", SplitLayoutPreset::TopLeft),
                        ("Top right", SplitLayoutPreset::TopRight),
                        ("Bottom left", SplitLayoutPreset::BottomLeft),
                        ("Bottom right", SplitLayoutPreset::BottomRight),
                    ] {
                        split_preset_button(
                            ui,
                            label,
                            preset,
                            self.availability.quadrants,
                            &mut self.keyboard_preset,
                            &mut selected,
                        );
                    }
                });
            });

        if let Some(preset) = selected {
            actions.push(UiAction::ApplySplitLayout(preset));
            open = false;
        }
        self.open = open;
    }
}

fn split_preset_button(
    ui: &mut egui::Ui,
    label: &str,
    preset: SplitLayoutPreset,
    enabled: bool,
    keyboard_preset: &mut Option<SplitLayoutPreset>,
    selected: &mut Option<SplitLayoutPreset>,
) {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(label)
            .selected(*keyboard_preset == Some(preset))
            .min_size(egui::vec2(88.0, 44.0)),
    );
    if response.hovered() {
        *keyboard_preset = Some(preset);
        response.clone().on_hover_ui(|ui| {
            ui.label(format!("Preview: {label}"));
            let (canvas, _) = ui.allocate_exact_size(egui::vec2(144.0, 81.0), egui::Sense::hover());
            let pane = split_preview_rect(canvas.shrink(3.0), preset);
            ui.painter()
                .rect_filled(canvas, 5.0, egui::Color32::from_rgb(24, 29, 39));
            ui.painter().rect_stroke(
                canvas,
                5.0,
                egui::Stroke::new(1.0, egui::Color32::from_gray(95)),
                egui::StrokeKind::Inside,
            );
            ui.painter()
                .rect_filled(pane, 3.0, egui::Color32::from_rgb(66, 153, 220));
        });
    }
    if response.clicked() {
        *selected = Some(preset);
    }
    if *keyboard_preset == Some(preset) {
        response.request_focus();
    }
}

fn split_available_presets(availability: SplitLayoutAvailability) -> Vec<SplitLayoutPreset> {
    let mut presets = Vec::new();
    if availability.focused_window && availability.side_by_side {
        presets.extend([SplitLayoutPreset::LeftHalf, SplitLayoutPreset::RightHalf]);
    }
    if availability.focused_window && availability.thirds {
        presets.extend([
            SplitLayoutPreset::LeftTwoThirds,
            SplitLayoutPreset::RightThird,
            SplitLayoutPreset::LeftThird,
            SplitLayoutPreset::RightTwoThirds,
        ]);
    }
    if availability.focused_window && availability.stacked {
        presets.extend([SplitLayoutPreset::TopHalf, SplitLayoutPreset::BottomHalf]);
    }
    if availability.focused_window && availability.quadrants {
        presets.extend([
            SplitLayoutPreset::TopLeft,
            SplitLayoutPreset::TopRight,
            SplitLayoutPreset::BottomLeft,
            SplitLayoutPreset::BottomRight,
        ]);
    }
    presets
}

fn move_selection(current: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

fn split_preview_rect(work: egui::Rect, preset: SplitLayoutPreset) -> egui::Rect {
    let x1 = work.left() + work.width() / 3.0;
    let x2 = work.left() + work.width() * 2.0 / 3.0;
    let xm = work.center().x;
    let ym = work.center().y;
    match preset {
        SplitLayoutPreset::LeftHalf => {
            egui::Rect::from_min_max(work.min, egui::pos2(xm, work.bottom()))
        }
        SplitLayoutPreset::RightHalf => {
            egui::Rect::from_min_max(egui::pos2(xm, work.top()), work.max)
        }
        SplitLayoutPreset::LeftTwoThirds => {
            egui::Rect::from_min_max(work.min, egui::pos2(x2, work.bottom()))
        }
        SplitLayoutPreset::RightThird => {
            egui::Rect::from_min_max(egui::pos2(x2, work.top()), work.max)
        }
        SplitLayoutPreset::LeftThird => {
            egui::Rect::from_min_max(work.min, egui::pos2(x1, work.bottom()))
        }
        SplitLayoutPreset::RightTwoThirds => {
            egui::Rect::from_min_max(egui::pos2(x1, work.top()), work.max)
        }
        SplitLayoutPreset::TopHalf => {
            egui::Rect::from_min_max(work.min, egui::pos2(work.right(), ym))
        }
        SplitLayoutPreset::BottomHalf => {
            egui::Rect::from_min_max(egui::pos2(work.left(), ym), work.max)
        }
        SplitLayoutPreset::TopLeft => egui::Rect::from_min_max(work.min, egui::pos2(xm, ym)),
        SplitLayoutPreset::TopRight => {
            egui::Rect::from_min_max(egui::pos2(xm, work.top()), egui::pos2(work.right(), ym))
        }
        SplitLayoutPreset::BottomLeft => {
            egui::Rect::from_min_max(egui::pos2(work.left(), ym), egui::pos2(xm, work.bottom()))
        }
        SplitLayoutPreset::BottomRight => egui::Rect::from_min_max(egui::pos2(xm, ym), work.max),
    }
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
            .fade_in(false)
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
            .fade_in(false)
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
                    let desired = egui::vec2(ui.available_width(), 104.0);
                    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
                    let animation = ui.ctx().animate_bool_with_time(
                        egui::Id::new(("workspace-thumbnail", entry.number)),
                        entry.active || response.hovered(),
                        0.18,
                    );
                    let background = egui::Color32::from_rgb(
                        (28.0 + 18.0 * animation) as u8,
                        (34.0 + 22.0 * animation) as u8,
                        (46.0 + 30.0 * animation) as u8,
                    );
                    ui.painter().rect_filled(rect, 7.0, background);
                    ui.painter().rect_stroke(
                        rect,
                        7.0,
                        egui::Stroke::new(
                            if entry.active { 2.0_f32 } else { 1.0_f32 },
                            if entry.active {
                                egui::Color32::from_rgb(84, 188, 255)
                            } else {
                                egui::Color32::from_gray(82)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    let preview = egui::Rect::from_min_max(
                        rect.min + egui::vec2(12.0, 30.0),
                        rect.max - egui::vec2(12.0, 10.0),
                    );
                    ui.painter()
                        .rect_filled(preview, 3.0, egui::Color32::from_rgb(12, 16, 24));
                    for (index, window) in entry.windows.iter().enumerate() {
                        let window_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                preview.left() + window.x * preview.width(),
                                preview.top() + window.y * preview.height(),
                            ),
                            egui::vec2(
                                (window.width * preview.width()).max(3.0),
                                (window.height * preview.height()).max(3.0),
                            ),
                        )
                        .intersect(preview);
                        let color = if index % 2 == 0 {
                            egui::Color32::from_rgb(54, 102, 142)
                        } else {
                            egui::Color32::from_rgb(76, 82, 116)
                        };
                        ui.painter().rect_filled(window_rect, 2.0, color);
                    }
                    ui.painter().text(
                        rect.min + egui::vec2(12.0, 8.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}  {}", entry.number, entry.name),
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                    let window_summary = match entry.windows.as_slice() {
                        [] => "Empty".to_string(),
                        [window] => window.title.clone(),
                        windows => format!("{} windows", windows.len()),
                    };
                    response.clone().on_hover_text(window_summary);
                    if response.clicked() {
                        selected = Some(entry.number);
                    }
                    ui.add_space(6.0);
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
            .fade_in(false)
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
                        if panel_close_button(ui).clicked() {
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
            .fade_in(false)
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
                        if panel_close_button(ui).clicked() {
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
            .fade_in(false)
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
                        if panel_close_button(ui).clicked() {
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
            .fade_in(false)
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
            .fade_in(false)
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
            .fade_in(false)
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
            if panel_close_button(ui).clicked() {
                *close_requested = true;
            }
        });
    });
}

/// A close control that stays visible even if the renderer loses egui's font
/// atlas. The icon is geometry painted directly into the mesh, not a glyph.
fn panel_close_button(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(6),
        visuals.bg_fill,
        egui::Stroke::new(1.0_f32, visuals.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    let inset = 7.0;
    let stroke = egui::Stroke::new(2.0_f32, visuals.fg_stroke.color);
    ui.painter().line_segment(
        [
            rect.left_top() + egui::vec2(inset, inset),
            rect.right_bottom() - egui::vec2(inset, inset),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            rect.right_top() + egui::vec2(-inset, inset),
            rect.left_bottom() + egui::vec2(inset, -inset),
        ],
        stroke,
    );
    response.on_hover_text("Close")
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
            .fade_in(false)
            .open(&mut self.open)
            .show(ctx, |ui| {
                ui.heading("Debug");
                ui.label(format!("Work area: {:?}", frame_ctx.work));
            });
    }
}

#[cfg(test)]
mod split_keyboard_tests {
    use super::{SplitLayoutAvailability, move_selection, split_available_presets};
    use crate::types::SplitLayoutPreset;

    #[test]
    fn keyboard_selection_wraps_in_both_directions() {
        assert_eq!(move_selection(0, 4, true), 1);
        assert_eq!(move_selection(3, 4, true), 0);
        assert_eq!(move_selection(0, 4, false), 3);
        assert_eq!(move_selection(2, 0, false), 0);
    }

    #[test]
    fn keyboard_navigation_skips_unavailable_layouts() {
        let presets = split_available_presets(SplitLayoutAvailability {
            focused_window: true,
            side_by_side: true,
            thirds: false,
            stacked: true,
            quadrants: false,
        });
        assert_eq!(
            presets,
            vec![
                SplitLayoutPreset::LeftHalf,
                SplitLayoutPreset::RightHalf,
                SplitLayoutPreset::TopHalf,
                SplitLayoutPreset::BottomHalf,
            ]
        );
    }
}
