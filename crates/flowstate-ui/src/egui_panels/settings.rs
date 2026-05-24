use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;
use flowstate_config::{FlowStateConfig, save_config};

// egui_panels/settings.rs
pub struct SettingsPanel {
    pub open: bool,
    tab: SettingsTab,
    config: FlowStateConfig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
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
            tab: SettingsTab::Appearance,
            config: FlowStateConfig::default(),
            open: false,
        }
    }
}

impl SettingsPanel {
fn show_appearance(&mut self, ui: &mut egui::Ui, _actions: &mut Vec<UiAction>) {
    ui.heading("Appearance");
    ui.separator();

    ui.label("Theme");
    ui.label("Density");
    ui.label("Font scale");
    ui.label("Chrome effects");
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
        
        egui::Window::new("FlowState Settings")
    .default_pos(egui::pos2(
        frame_ctx.work.loc.x as f32 + 24.0,
        frame_ctx.work.loc.y as f32 + 24.0,
    ))
    .default_width(850.0)
    .default_height(600.0)
    .show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(170.0);

                self.nav_button(ui, SettingsTab::Appearance, "Appearance");
                self.nav_button(ui, SettingsTab::Displays, "Displays");
                self.nav_button(ui, SettingsTab::Workspaces, "Workspaces");
                self.nav_button(ui, SettingsTab::Keyboard, "Keyboard");
                self.nav_button(ui, SettingsTab::Privacy, "Privacy");
                self.nav_button(ui, SettingsTab::Power, "Power");
                self.nav_button(ui, SettingsTab::Debug, "Debug");
                self.nav_button(ui, SettingsTab::About, "About");
            });

            ui.separator();

            ui.vertical(|ui| {
                ui.set_min_width(600.0);

                match self.tab {
                    SettingsTab::Appearance => self.show_appearance(ui, actions),
                    SettingsTab::Displays => self.show_displays(ui, actions),
                    SettingsTab::Workspaces => self.show_placeholder(ui, "Workspaces"),
                    SettingsTab::Keyboard => self.show_placeholder(ui, "Keyboard"),
                    SettingsTab::Privacy => self.show_placeholder(ui, "Privacy"),
                    SettingsTab::Power => self.show_placeholder(ui, "Power"),
                    SettingsTab::Debug => self.show_debug(ui, actions),
                    SettingsTab::About => self.show_placeholder(ui, "About"),
                }
            });
        });
    });
}


fn nav_button(&mut self, ui: &mut egui::Ui, tab: SettingsTab, label: &str) {
    if ui
        .selectable_label(self.tab == tab, label)
        .clicked()
    {
        self.tab = tab;
    }
}

fn show_placeholder(&mut self, ui: &mut egui::Ui, title: &str) {
    ui.heading(title);
    ui.label(format!("{title} settings"));
}
    
/*
impl SettingsPanel {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        egui::Window::new("FlowState Settings")
            .default_pos(egui::pos2(
                frame_ctx.work.loc.x as f32 + 24.0,
                frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Appearance").clicked() {
                        self.tab = SettingsTab::Appearance;
                    }
                    if ui.button("Displays").clicked() {
                        self.tab = SettingsTab::Displays;
                    }
                    if ui.button("Keyboard").clicked() {
                        self.tab = SettingsTab::Keyboard;
                    }
                });

                ui.separator();

                match self.tab {
                    SettingsTab::Appearance => self.show_appearance(ui, actions),
                    SettingsTab::Displays => self.show_displays(ui, actions),
                    SettingsTab::Keyboard => self.show_keyboard(ui, actions),
                    SettingsTab::Privacy => self.show_privacy(ui, actions),
                    SettingsTab::Debug => self.show_debug(ui, actions),
                }
            });
    }


    fn show_appearance(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    
    ui.heading("Appearance");

    let mut changed = false;

    changed |= ui.checkbox(
        &mut self.config.appearance.shader_chrome,
        "Use shader chrome",
    ).changed();

    changed |= ui.checkbox(
        &mut self.config.appearance.output_focus_glow,
        "Output focus glow",
    ).changed();

    changed |= ui.add(
        egui::Slider::new(
            &mut self.config.appearance.glow_strength,
            0.0..=1.0,
        ).text("Glow strength")
    ).changed();

    changed |= ui.add(
        egui::Slider::new(
            &mut self.config.appearance.font_scale,
            0.75..=1.5,
        ).text("Font scale")
    ).changed();

    egui::ComboBox::from_label("Theme")
        .selected_text(&self.config.appearance.theme)
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(
                &mut self.config.appearance.theme,
                "Eagle".to_string(),
                "Eagle",
            ).changed();

            changed |= ui.selectable_value(
                &mut self.config.appearance.theme,
                "Moonbase".to_string(),
                "Moonbase",
            ).changed();

            changed |= ui.selectable_value(
                &mut self.config.appearance.theme,
                "Classic".to_string(),
                "Classic",
            ).changed();
        });

    if changed {
        let _ = save_config(&self.config);
        actions.push(UiAction::ReloadConfig);
    }
}
*/

    fn show_displays(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        ui.heading("Displays");
    }

    fn show_keyboard(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        ui.heading("Keyboard");
    }

    fn show_privacy(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        ui.heading("Privacy");
    }

    fn show_debug(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        ui.heading("Debug");
    }
}
