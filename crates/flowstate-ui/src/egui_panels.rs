use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiAction;

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

#[derive(Default)]
pub struct SettingsPanel {
    pub open: bool,
}

#[derive(Default)]
pub struct LauncherPanel {
    pub open: bool,
}

#[derive(Default)]
pub struct DebugPanel {
    pub open: bool,
}

impl EguiPanelView for SettingsPanel {
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

impl EguiPanelView for LauncherPanel {
    fn title(&self) -> &'static str {
        "Launcher"
    }
    fn show(
        &mut self,
        ctx: &egui::Context,
        _frame_ctx: &DesktopFrameCtx,
        actions: &mut Vec<UiAction>,
    ) {
        if !self.open {
            return;
        }

        egui::Window::new("Launcher")
            .default_pos(egui::pos2(
                _frame_ctx.work.loc.x as f32 + 24.0,
                _frame_ctx.work.loc.y as f32 + 24.0,
            ))
            .open(&mut self.open)
            .show(ctx, |ui| {
                if ui.button("Terminal").clicked() {
                    actions.push(UiAction::LaunchApp("weston-terminal"));
                }
            });
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
