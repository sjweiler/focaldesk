
// egui_panels/settings.rs
pub struct SettingsPanel {
    pub open: bool,
    tab: SettingsTab,
}

enum SettingsTab {
    Appearance,
    Displays,
    Keyboard,
    Privacy,
    Debug,
}

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

        if ui.button("Eagle").clicked() {
            actions.push(UiAction::SetTheme(/* Eagle */));
        }

        if ui.button("Moonbase").clicked() {
            actions.push(UiAction::SetTheme(/* Moonbase */));
        }
    }

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
