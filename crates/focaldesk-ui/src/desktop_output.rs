use std::time::Instant;

use focaldesk_themes::{FlowTheme, FlowThemeId};
use focaldesk_types::OutputId;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::Texture;
use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::utils::{Logical, Physical, Rectangle, Size};

use crate::atlas::IconAtlas;
use crate::chrome::ChromeMetrics;
use crate::chrome_layout::{ChromeLayout, build_chrome_layout};
use crate::chrome_shaders::ChromeShaders;
use crate::desktop_frame::DesktopFrameCtx;
use crate::dialog::DialogId;
use crate::dialog_layer::DialogLayer;
use crate::egui_layer::EguiLayer;
use crate::element::UiElement;
use crate::overlay::OverlayManager;
use crate::sidebar::Dock;
use crate::topbar::SystemPanel;
use crate::uicomponent::{LayoutCtx, UiComponent, UiHit};
use crate::uitree::UiTree;
use crate::workarea::WorkArea;
use smithay::utils::Point;

pub struct DesktopOutputConfig {
    pub show_topbar: bool,
    pub show_sidebar: bool,
    pub theme_id: FlowThemeId,
}

pub struct DesktopOutput {
    pub output_id: OutputId,
    pub logical_rect: Rectangle<i32, Logical>,
    pub scale_factor: f64,
    pub config: DesktopOutputConfig,
    pub metrics: ChromeMetrics,
    pub chrome_layout: ChromeLayout,
    pub system_panel: Option<SystemPanel>,
    pub dock: Option<Dock>,
    pub workarea: WorkArea,
    pub overlays: Option<OverlayManager>,
    pub dialog: Option<DialogLayer>,
    pub egui: EguiLayer,
    pub chrome_shaders: ChromeShaders,
    pub render_start: Instant,
}

impl DesktopOutput {
    pub fn new(output_id: OutputId, config: DesktopOutputConfig) -> Self {
        Self {
            output_id,
            logical_rect: Rectangle::from_loc_and_size((0, 0), (1, 1)),
            scale_factor: 1.0,
            config,
            metrics: ChromeMetrics::default(),
            chrome_layout: build_chrome_layout(Size::from((1, 1)), 64, 76),
            system_panel: Some(SystemPanel::default()),
            dock: Some(Dock::default()),
            workarea: WorkArea::new(),
            overlays: Some(OverlayManager::default()),
            dialog: None,
            egui: EguiLayer::default(),
            chrome_shaders: ChromeShaders::new(),
            render_start: Instant::now(),
        }
    }

    /// Recompute chrome layout and per-component bounds (top bar, sidebar, work area).
    pub fn layout(
        &mut self,
        rect: Rectangle<i32, Logical>,
        _theme: &FlowTheme,
        ui_tree: &mut UiTree,
    ) {
        self.logical_rect = rect;
        let top_h = if self.config.show_topbar {
            self.metrics.topbar_h
        } else {
            0
        };
        let left_w = if self.config.show_sidebar {
            self.metrics.sidebar_w
        } else {
            0
        };

        let options = crate::ui_builder::UiBuildOptions::default();
        self.chrome_layout = crate::chrome_layout::build_chrome_layout_with_config(
            rect.size,
            top_h,
            left_w,
            options.layout_config(),
        );
        crate::ui_builder::build_ui_for_output_with_options(ui_tree, &self.chrome_layout, options);

        self.update_components_from_ui_tree(ui_tree);

        let layout_ctx = LayoutCtx {
            screen: rect,
            scale: self.scale_factor as f32,
        };

        if let Some(system_panel) = &mut self.system_panel {
            system_panel.layout_from_chrome(&self.chrome_layout, &layout_ctx);
        }
        if let Some(dock) = &mut self.dock {
            dock.layout_from_chrome(&self.chrome_layout, &layout_ctx);
        }
        self.workarea
            .layout_from_chrome(&self.chrome_layout, &layout_ctx);
    }

    pub fn ensure_shaders(
        &mut self,
        renderer: &mut GlesRenderer,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        self.chrome_shaders.ensure_compiled(renderer)
    }

    /// Synchronize the output-owned chrome model from the compositor's current
    /// layout and accessibility projection.
    pub fn sync_chrome(
        &mut self,
        logical_rect: Rectangle<i32, Logical>,
        scale_factor: f64,
        metrics: ChromeMetrics,
        chrome_layout: ChromeLayout,
        ui_tree: &UiTree,
    ) {
        self.logical_rect = logical_rect;
        self.scale_factor = scale_factor;
        self.metrics = metrics;
        self.chrome_layout = chrome_layout;
        self.update_components_from_ui_tree(ui_tree);

        let layout_ctx = LayoutCtx {
            screen: logical_rect,
            scale: scale_factor as f32,
        };
        if let Some(system_panel) = &mut self.system_panel {
            system_panel.layout_from_chrome(&self.chrome_layout, &layout_ctx);
        }
        if let Some(dock) = &mut self.dock {
            dock.layout_from_chrome(&self.chrome_layout, &layout_ctx);
        }
        self.workarea
            .layout_from_chrome(&self.chrome_layout, &layout_ctx);
    }

    pub fn chrome_elements(&self) -> impl Iterator<Item = &UiElement> {
        self.dock
            .iter()
            .flat_map(|dock| dock.elements.iter())
            .chain(
                self.system_panel
                    .iter()
                    .flat_map(|panel| panel.elements.iter()),
            )
    }

    pub fn sidebar_hover_slot(&self) -> Option<usize> {
        self.dock
            .as_ref()?
            .elements
            .iter()
            .position(|element| element.hovered)
    }

    pub fn clear_chrome_hover(&mut self) {
        if let Some(system_panel) = &mut self.system_panel {
            system_panel.clear_hover();
        }
        if let Some(dock) = &mut self.dock {
            dock.clear_hover();
        }
    }

    pub fn update_chrome_hover(&mut self, point: Point<i32, Logical>) {
        if let Some(system_panel) = &mut self.system_panel {
            system_panel.update_hover(point);
        }
        if let Some(dock) = &mut self.dock {
            dock.update_hover(point);
        }
    }

    /// Rebuild component-owned chrome content when layout/content changes.
    /// Rendering does not copy the UiTree every frame.
    fn update_components_from_ui_tree(&mut self, ui_tree: &UiTree) {
        if let Some(system_panel) = &mut self.system_panel {
            system_panel.set_elements(
                ui_tree
                    .elements
                    .iter()
                    .filter(|element| {
                        matches!(
                            element.kind,
                            crate::types::UiElementKind::TopbarIndicator
                                | crate::types::UiElementKind::TopbarButton
                                | crate::types::UiElementKind::TopbarFlowField
                                | crate::types::UiElementKind::Clock
                        )
                    })
                    .cloned()
                    .collect(),
            );
        }
        if let Some(dock) = &mut self.dock {
            dock.set_elements(
                ui_tree
                    .elements
                    .iter()
                    .filter(|element| {
                        matches!(
                            element.kind,
                            crate::types::UiElementKind::SidebarButton
                                | crate::types::UiElementKind::WorkspaceSlot
                        )
                    })
                    .cloned()
                    .collect(),
            );
        }
    }

    /// Update hover state in the component-owned chrome model. Call this from
    /// pointer-motion handling before scheduling a redraw.
    pub fn update_pointer(&mut self, point: Point<i32, Logical>) -> bool {
        let hit = self.hit_test(point);
        let mut changed = false;
        match hit.map(|hit| hit.target) {
            Some(crate::uicomponent::UiHitTarget::SystemPanel) => {
                if let Some(system_panel) = &mut self.system_panel {
                    changed |= system_panel.update_hover(point);
                }
                if let Some(dock) = &mut self.dock {
                    changed |= dock.clear_hover();
                }
            }
            Some(crate::uicomponent::UiHitTarget::Dock) => {
                if let Some(system_panel) = &mut self.system_panel {
                    changed |= system_panel.clear_hover();
                }
                if let Some(dock) = &mut self.dock {
                    changed |= dock.update_hover(point);
                }
            }
            _ => {
                if let Some(system_panel) = &mut self.system_panel {
                    changed |= system_panel.clear_hover();
                }
                if let Some(dock) = &mut self.dock {
                    changed |= dock.clear_hover();
                }
            }
        }
        changed
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render<'a>(
        &mut self,
        frame: &mut GlesFrame<'a, 'a>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        ui_tree: &UiTree,
        sidebar_hover_slot: Option<usize>,
        active_dialog: Option<DialogId>,
        draw_dialog_on_this_output: bool,
        wallpaper_texture: Option<&GlesTexture>,
        icon_atlas: Option<&IconAtlas>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        self.render_background(frame, frame_ctx, theme, damage)?;
        self.render_wallpaper(frame, frame_ctx, wallpaper_texture, damage)?;
        self.render_bottom_chrome(frame, frame_ctx, theme, sidebar_hover_slot)?;
        self.render_workarea();
        // Client surfaces are composited by the engine between work area and top chrome.
        self.render_top_chrome(
            frame,
            frame_ctx,
            theme,
            ui_tree,
            sidebar_hover_slot,
            icon_atlas,
        )?;
        self.render_dialogs(
            frame,
            frame_ctx,
            theme,
            active_dialog,
            draw_dialog_on_this_output,
        )?;
        self.render_overlays(frame, frame_ctx, damage)?;
        self.render_egui(frame, frame_ctx, theme, damage)?;
        Ok(())
    }

    fn render_background(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        let c = theme.background.color;
        frame.clear(Color32F::new(c[0], c[1], c[2], c[3]), damage)
    }

    fn render_wallpaper(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        wallpaper_texture: Option<&GlesTexture>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        let Some(tex) = wallpaper_texture else {
            return Ok(());
        };
        let target = self.chrome_layout.work_area.recess;
        let target_physical = target.to_physical_precise_round(frame_ctx.output_scale);
        let sz = tex.size();
        if sz.w == 0 || sz.h == 0 {
            return Ok(());
        }
        use smithay::utils::Transform;
        let src =
            smithay::utils::Rectangle::from_loc_and_size((0.0, 0.0), (sz.w as f64, sz.h as f64));
        let damage_local = [Rectangle::from_loc_and_size(
            (0, 0),
            (target_physical.size.w, target_physical.size.h),
        )];
        let _ = frame.render_texture_from_to(
            tex,
            src,
            target_physical,
            &damage_local,
            damage,
            Transform::Normal,
            1.0,
            None,
            &[],
        );
        Ok(())
    }

    /// Structural top bar / sidebar / work-area shell (under wallpaper and clients).
    fn render_bottom_chrome<'a>(
        &self,
        frame: &mut GlesFrame<'a, 'a>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        sidebar_hover_slot: Option<usize>,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        crate::chrome_draw::draw_chrome_workarea_frame(
            frame,
            &self.chrome_shaders,
            frame_ctx,
            &self.chrome_layout,
            &theme.chrome,
        );
        let mut ctx = crate::uicomponent::RenderCtx {
            frame,
            frame_ctx,
            damage: &[],
            output_scale: frame_ctx.output_scale.x,
            output_id: self.output_id,
            shaders: &self.chrome_shaders,
            theme,
            atlas: None,
            chrome_layout: &self.chrome_layout,
            metrics: &self.metrics,
            active_dialog: None,
            draw_on_this_output: true,
        };
        if let Some(system_panel) = &self.system_panel {
            system_panel.render(&mut ctx)?;
        }
        if let Some(dock) = &self.dock {
            dock.render(&mut ctx)?;
        }
        let _ = sidebar_hover_slot;
        Ok(())
    }

    fn render_workarea(&self) {
        let _ = &self.workarea;
    }

    /// Trim, glass tint, and chrome icons above client surfaces.
    fn render_top_chrome<'a>(
        &self,
        frame: &mut GlesFrame<'a, 'a>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        ui_tree: &UiTree,
        sidebar_hover_slot: Option<usize>,
        icon_atlas: Option<&IconAtlas>,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        crate::chrome_draw::draw_chrome_trim_glass(
            frame,
            &self.chrome_shaders,
            frame_ctx,
            &self.chrome_layout,
            theme,
        )?;

        let mut ctx = crate::uicomponent::RenderCtx {
            frame,
            frame_ctx,
            damage: &[],
            output_scale: frame_ctx.output_scale.x,
            output_id: self.output_id,
            shaders: &self.chrome_shaders,
            theme,
            atlas: icon_atlas,
            chrome_layout: &self.chrome_layout,
            metrics: &self.metrics,
            active_dialog: None,
            draw_on_this_output: true,
        };
        if let Some(system_panel) = &self.system_panel {
            system_panel.render_icons(&mut ctx)?;
        }
        if let Some(dock) = &self.dock {
            dock.render_icons(&mut ctx)?;
        }
        let _ = (ui_tree, sidebar_hover_slot);
        Ok(())
    }

    fn render_dialogs(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        active_dialog: Option<DialogId>,
        draw_on_this_output: bool,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        let Some(layer) = &self.dialog else {
            return Ok(());
        };
        layer.render(
            frame,
            frame_ctx,
            &self.chrome_shaders,
            theme,
            active_dialog,
            draw_on_this_output,
        )
    }

    fn render_overlays(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        if let Some(overlays) = &self.overlays {
            overlays.render(frame, frame_ctx, damage);
        }
        Ok(())
    }

    /// egui is composited last so debug / tooling UI stays on top.
    fn render_egui(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        self.egui
            .render(frame, frame_ctx, damage, &self.chrome_shaders, theme)
    }

    pub fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        if let Some(dialog) = &self.dialog
            && let Some(hit) = dialog.hit_test(point)
        {
            return Some(hit);
        }

        if let Some(overlays) = &self.overlays
            && let Some(hit) = overlays.hit_test(point)
        {
            return Some(hit);
        }

        if let Some(system_panel) = &self.system_panel
            && let Some(hit) = system_panel.hit_test(point)
        {
            return Some(hit);
        }

        if let Some(dock) = &self.dock
            && let Some(hit) = dock.hit_test(point)
        {
            return Some(hit);
        }

        self.workarea.hit_test(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PanelKind;

    fn output(id: u64) -> DesktopOutput {
        DesktopOutput::new(
            OutputId(id),
            DesktopOutputConfig {
                show_topbar: true,
                show_sidebar: true,
                theme_id: FlowThemeId::default(),
            },
        )
    }

    #[test]
    fn egui_panel_state_is_independent_per_output() {
        let mut first = output(1);
        let mut second = output(2);

        first.egui.open_panel(PanelKind::Settings, first.output_id);
        second.egui.open_panel(PanelKind::Power, second.output_id);

        assert!(first.egui.is_open_on_output(first.output_id));
        assert!(second.egui.is_open_on_output(second.output_id));

        first.egui.close_all_panels();
        assert!(!first.egui.has_open_panels());
        assert!(second.egui.has_open_panels());
    }
}
