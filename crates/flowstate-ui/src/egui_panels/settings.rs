use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;
use flowstate_config::{FlowStateConfig, save_config};

fn sidebar_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let fill = if selected {
        egui::Color32::from_rgb(18, 78, 130)
    } else {
        egui::Color32::TRANSPARENT
    };

    let text_color = if selected {
        egui::Color32::from_rgb(120, 200, 255)
    } else {
        egui::Color32::from_rgb(210, 220, 230)
    };

    ui.add_sized(
        [160.0, 34.0],
        egui::Button::new(egui::RichText::new(text).color(text_color).size(15.0))
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(8))
            .frame(true),
    )
}

pub struct SettingsPanel {
    pub open: bool,
    tab: SettingsPage,
    config: FlowStateConfig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Displays,
    Workspaces,
    Keyboard,
    Privacy,
    Power,
    Debug,
    About,
}

impl Default for SettingsPanel {
    fn default() -> Self {
        Self {
            tab: SettingsPage::Appearance,
            config: FlowStateConfig::default(),
            open: false,
        }
    }
}

impl SettingsPanel {
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.separator();
        ui.add_space(8.0);

        if sidebar_button(ui, "Appearance", self.tab == SettingsPage::Appearance).clicked() {
            self.tab = SettingsPage::Appearance;
        }

        if sidebar_button(ui, "Displays", self.tab == SettingsPage::Displays).clicked() {
            self.tab = SettingsPage::Displays;
        }

        if sidebar_button(ui, "Workspaces", self.tab == SettingsPage::Workspaces).clicked() {
            self.tab = SettingsPage::Workspaces;
        }

        if sidebar_button(ui, "Keyboard", self.tab == SettingsPage::Keyboard).clicked() {
            self.tab = SettingsPage::Keyboard;
        }

        if sidebar_button(ui, "Privacy", self.tab == SettingsPage::Privacy).clicked() {
            self.tab = SettingsPage::Privacy;
        }

        if sidebar_button(ui, "Power", self.tab == SettingsPage::Power).clicked() {
            self.tab = SettingsPage::Power;
        }

        if sidebar_button(ui, "Debug", self.tab == SettingsPage::Debug).clicked() {
            self.tab = SettingsPage::Debug;
        }

        if sidebar_button(ui, "About", self.tab == SettingsPage::About).clicked() {
            self.tab = SettingsPage::About;
        }
    }

    fn displays_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Displays");

        ui.checkbox(
            &mut self.config.displays.topbar_on_all_outputs,
            "Top bar on all outputs",
        );

        ui.checkbox(
            &mut self.config.displays.sidebar_on_all_outputs,
            "Sidebar on all outputs",
        );

        ui.checkbox(
            &mut self.config.displays.remember_focused_output,
            "Remember focused output",
        );
    }

    fn appearance_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appearance");

        let mut changed = false;

        changed |= ui
            .checkbox(
                &mut self.config.appearance.shader_chrome,
                "Use shader chrome",
            )
            .changed();

        changed |= ui
            .checkbox(
                &mut self.config.appearance.output_focus_glow,
                "Output focus glow",
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.config.appearance.glow_strength, 0.0..=1.0)
                    .text("Glow strength"),
            )
            .changed();

        changed |= ui
            .add(
                egui::Slider::new(&mut self.config.appearance.font_scale, 0.75..=1.5)
                    .text("Font scale"),
            )
            .changed();

        if changed {
            let _ = save_config(&self.config);
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        _actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        let mut open = self.open;
        let mut close_requested = false;
        let response = egui::Window::new("FlowState Settings")
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .default_size(egui::vec2(900.0, 430.0))
            .resizable(true)
            .collapsible(false)
            .title_bar(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Settings");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.set_height(ui.available_height());

                    ui.vertical(|ui| {
                        ui.set_width(180.0);
                        self.sidebar(ui);
                    });

                    ui.separator();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(12.0);
                            match self.tab {
                                SettingsPage::Appearance => self.appearance_page(ui),
                                SettingsPage::Displays => self.displays_page(ui),
                                SettingsPage::Workspaces => self.show_placeholder(ui, "Workspaces"),
                                SettingsPage::Keyboard => self.show_placeholder(ui, "Keyboard"),
                                SettingsPage::Privacy => self.show_placeholder(ui, "Privacy"),
                                SettingsPage::Power => self.show_placeholder(ui, "Power"),
                                SettingsPage::Debug => self.show_placeholder(ui, "Debug"),
                                SettingsPage::About => self.show_placeholder(ui, "About"),
                            }
                        });
                });
            });

        if close_requested || response.is_none() || !open {
            self.open = false;
        }
    }

    fn show_placeholder(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.heading(title);
        ui.label(format!("{title} settings"));
    }
}
