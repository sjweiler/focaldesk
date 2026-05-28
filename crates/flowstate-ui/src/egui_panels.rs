use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;
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
                ui.heading("FlowState Settings");
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
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.small_button("✕").clicked() {
                                close_requested = true;
                            }
                        },
                    );
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
