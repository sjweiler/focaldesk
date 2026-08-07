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
use crate::chrome_draw::{draw_chrome_below_work_wallpaper, draw_chrome_trim_glass_icons};
use crate::chrome_layout::{ChromeLayout, build_chrome_layout};
use crate::chrome_shaders::ChromeShaders;
use crate::desktop_frame::DesktopFrameCtx;
use crate::dialog::DialogId;
use crate::dialog_layer::DialogLayer;
use crate::egui_layer::EguiLayer;
use crate::overlay::OverlayManager;
use crate::sidebar::SideBar;
use crate::topbar::TopBar;
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
    pub topbar: Option<TopBar>,
    pub sidebar: Option<SideBar>,
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
            topbar: Some(TopBar::default()),
            sidebar: Some(SideBar::default()),
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

        let layout_ctx = LayoutCtx {
            screen: rect,
            scale: self.scale_factor as f32,
        };

        if let Some(topbar) = &mut self.topbar {
            topbar.layout_from_chrome(&self.chrome_layout, &layout_ctx);
        }
        if let Some(sidebar) = &mut self.sidebar {
            sidebar.layout_from_chrome(&self.chrome_layout, &layout_ctx);
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

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
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
    fn render_bottom_chrome(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        sidebar_hover_slot: Option<usize>,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        draw_chrome_below_work_wallpaper(
            frame,
            &self.chrome_shaders,
            frame_ctx,
            &self.chrome_layout,
            sidebar_hover_slot,
            &theme.chrome,
        );
        Ok(())
    }

    fn render_workarea(&self) {
        let _ = &self.workarea;
    }

    /// Trim, glass tint, and chrome icons above client surfaces.
    fn render_top_chrome(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        theme: &FlowTheme,
        ui_tree: &UiTree,
        sidebar_hover_slot: Option<usize>,
        icon_atlas: Option<&IconAtlas>,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        draw_chrome_trim_glass_icons(
            frame,
            &self.chrome_shaders,
            frame_ctx,
            &self.chrome_layout,
            &self.metrics,
            ui_tree,
            theme,
            sidebar_hover_slot,
            icon_atlas,
        )
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

        if let Some(topbar) = &self.topbar
            && let Some(hit) = topbar.hit_test(point)
        {
            return Some(hit);
        }

        if let Some(sidebar) = &self.sidebar
            && let Some(hit) = sidebar.hit_test(point)
        {
            return Some(hit);
        }

        self.workarea.hit_test(point)
    }
}
