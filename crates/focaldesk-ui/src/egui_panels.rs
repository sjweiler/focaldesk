use crate::desktop_frame::DesktopFrameCtx;
use crate::types::{SettingKey, SystemCommand, UiAction};
use chrono::{Datelike, Local, NaiveDate};
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
pub struct LauncherPanel {
    pub open: bool,
}

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

#[derive(Default)]
pub struct CalendarPanel {
    pub open: bool,
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

impl EguiPanelView for LauncherPanel {
    fn title(&self) -> &'static str {
        "Launcher"
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

        let mut open = self.open;
        let mut close_requested = false;
        let response = egui::Window::new("Launcher")
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Launcher");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                if ui.button("Terminal").clicked() {
                    actions.push(UiAction::LaunchApp("@terminal"));
                }
                if ui.button("Browser").clicked() {
                    actions.push(UiAction::LaunchApp("@browser"));
                }
                if ui.button("Files").clicked() {
                    actions.push(UiAction::LaunchApp("@files"));
                }
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }
}

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
