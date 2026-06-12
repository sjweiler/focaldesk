//! egui overlay — rendered last, above dialogs and compositor chrome.

use std::{mem, sync::Arc};

use egui::{
    ClippedPrimitive, Context, Event, FontData, FontDefinitions, FontFamily, ImageData, Modifiers,
    MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, TextureId, TexturesDelta, Vec2,
    epaint::Primitive,
};
use egui_glow::Painter;
use focaldesk_logging::{flog_error, flog_info};
use focaldesk_themes::FlowTheme;
use focaldesk_types::OutputId;
use glow;
use smithay::backend::egl::get_proc_address;
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesPixelProgram, Uniform};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size};

use crate::chrome_shaders::ChromeShaders;
use crate::desktop_frame::DesktopFrameCtx;
use crate::egui_panels::{
    AudioPanel, BluetoothPanel, CalendarPanel, DebugPanel, EguiPanelView, LauncherPanel,
    NetworkPanel, PowerPanel, SettingsPanel,
};
use crate::types::{PanelKind, UiAction};

pub struct EguiLayer {
    ctx: Context,
    raw_input: RawInput,
    actions: Vec<UiAction>,
    settings: SettingsPanel,
    launcher: LauncherPanel,
    network: NetworkPanel,
    bluetooth: BluetoothPanel,
    audio: AudioPanel,
    power: PowerPanel,
    calendar: CalendarPanel,
    debug: DebugPanel,

    textures_delta: TexturesDelta,
    primitives: Vec<ClippedPrimitive>,
    glow_painter: Option<Painter>,
    wants_pointer_input: bool,
    wants_keyboard_input: bool,
    logged_texture_delta: bool,
    logged_mesh_sample: bool,
    dumped_font_atlas: bool,
    dumped_font_mesh: bool,
    last_font_atlas_rgba: Option<Vec<u8>>,
    last_pointer_pos: Option<Pos2>,
    owner_output: Option<OutputId>,
    pub screen_height_pts: f32,
    pub last_frame_ctx: Option<DesktopFrameCtx>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EguiInputEvent {
    PointerMoved {
        position: Point<f64, Logical>,
    },
    PointerButton {
        button: EguiPointerButton,
        pressed: bool,
        position: Point<f64, Logical>,
        modifiers: EguiModifiers,
    },
    PointerScroll {
        delta: EguiScrollDelta,
        position: Point<f64, Logical>,
        modifiers: EguiModifiers,
    },
    PointerGone,
    Key {
        key: Option<egui::Key>,
        pressed: bool,
        repeat: bool,
        modifiers: EguiModifiers,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EguiPointerButton {
    Primary,
    Secondary,
    Middle,
    Extra1,
    Extra2,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EguiModifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub mac_cmd: bool,
    pub command: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EguiScrollDelta {
    Line { x: f32, y: f32 },
    Point { x: f32, y: f32 },
}

pub fn apply_focaldesk_egui_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.window_corner_radius = egui::CornerRadius::same(14);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 18],
        blur: 40,
        spread: 0,
        color: egui::Color32::from_black_alpha(160),
    };

    visuals.panel_fill = egui::Color32::from_rgba_premultiplied(6, 14, 28, 220);
    visuals.window_fill = egui::Color32::from_rgba_premultiplied(8, 18, 34, 230);
    visuals.extreme_bg_color = egui::Color32::from_rgb(5, 10, 18);
    visuals.faint_bg_color = egui::Color32::from_rgb(10, 25, 45);

    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(14, 32, 56);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(20, 72, 120);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(28, 115, 190);

    visuals.selection.bg_fill = egui::Color32::from_rgb(28, 135, 220);
    visuals.hyperlink_color = egui::Color32::from_rgb(80, 170, 255);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(14.0, 12.0);
    style.spacing.window_margin = egui::Margin::same(18);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    ctx.set_style(style);
}

impl Default for EguiLayer {
    fn default() -> Self {
        let ctx = Context::default();
        ctx.set_fonts(focaldesk_egui_fonts());
        apply_focaldesk_egui_style(&ctx);

        Self {
            ctx,
            raw_input: RawInput::default(),
            actions: Vec::new(),
            textures_delta: TexturesDelta::default(),
            primitives: Vec::new(),
            glow_painter: None,
            wants_pointer_input: false,
            wants_keyboard_input: false,
            logged_texture_delta: false,
            logged_mesh_sample: false,
            dumped_font_atlas: false,
            dumped_font_mesh: false,
            last_font_atlas_rgba: None,
            last_pointer_pos: None,
            owner_output: None,
            screen_height_pts: 1.0,
            last_frame_ctx: None,
            settings: SettingsPanel::default(),
            launcher: LauncherPanel::default(),
            network: NetworkPanel::default(),
            bluetooth: BluetoothPanel::default(),
            audio: AudioPanel::default(),
            power: PowerPanel::default(),
            calendar: CalendarPanel::default(),
            debug: DebugPanel::default(),
        }
    }
}

fn focaldesk_egui_fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "FocalDeskSans".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/IBMPlexSans-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "FocalDeskMono".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/IBMPlexMono-Regular.ttf"
        ))),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "FocalDeskSans".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "FocalDeskMono".to_owned());

    fonts
}

/// Gives egui overlay code access to FocalDesk's compiled shader set and current GLES frame.
///
/// This is intentionally thin: egui can own interaction/layout, while compositor-native effects
/// still render through the same `ChromeShaders` programs used by the shell.
pub struct EguiShaderBridge<'a, 'frame, 'buffer> {
    pub frame: &'a mut GlesFrame<'frame, 'buffer>,
    pub frame_ctx: &'a DesktopFrameCtx,
    pub damage: &'a [Rectangle<i32, Physical>],
    pub shaders: &'a ChromeShaders,
    pub theme: &'a FlowTheme,
}

impl<'a, 'frame, 'buffer> EguiShaderBridge<'a, 'frame, 'buffer> {
    pub fn draw_pixel_shader(
        &mut self,
        program: &GlesPixelProgram,
        rect: Rectangle<i32, Logical>,
        uniforms: &[Uniform<'_>],
    ) -> Result<(), GlesError> {
        let rect_physical: Rectangle<i32, Physical> =
            rect.to_physical_precise_round(self.frame_ctx.output_scale);
        let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );
        let buffer_size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));

        self.frame.render_pixel_shader_to(
            program,
            src_rect,
            rect_physical,
            buffer_size,
            Some(self.damage),
            1.0,
            uniforms,
        )
    }

    pub fn draw_rounded_rect(
        &mut self,
        rect: Rectangle<i32, Logical>,
        radius: f32,
        color: [f32; 4],
    ) -> Result<(), GlesError> {
        let Some(program) = self.shaders.rounded_rect.as_ref() else {
            return Ok(());
        };

        let rect_physical: Rectangle<i32, Physical> =
            rect.to_physical_precise_round(self.frame_ctx.output_scale);
        self.draw_pixel_shader(
            program,
            rect,
            &[
                Uniform::new(
                    "u_size",
                    [rect_physical.size.w as f32, rect_physical.size.h as f32],
                ),
                Uniform::new("u_radius", radius),
                Uniform::new("u_color", color),
            ],
        )
    }
}

impl EguiLayer {
    pub fn has_open_panels(&self) -> bool {
        self.settings.open
            || self.launcher.open
            || self.network.open
            || self.bluetooth.open
            || self.audio.open
            || self.power.open
            || self.calendar.open
            || self.debug.open
    }

    pub fn owner_output(&self) -> Option<OutputId> {
        self.owner_output
    }

    pub fn is_open_on_output(&self, output: OutputId) -> bool {
        self.has_open_panels() && self.owner_output == Some(output)
    }

    pub fn open_panel(&mut self, panel: PanelKind, owner_output: OutputId) {
        let mut opened = false;
        match panel {
            PanelKind::Settings => {
                self.settings.open = !self.settings.open;
                opened = self.settings.open;
            }
            PanelKind::AppLauncher => {
                self.launcher.open = !self.launcher.open;
                opened = self.launcher.open;
            }
            PanelKind::Network => {
                self.network.open = !self.network.open;
                opened = self.network.open;
            }
            PanelKind::Bluetooth => {
                self.bluetooth.open = !self.bluetooth.open;
                opened = self.bluetooth.open;
            }
            PanelKind::Audio => {
                self.audio.open = !self.audio.open;
                opened = self.audio.open;
            }
            PanelKind::Power => {
                self.power.open = !self.power.open;
                opened = self.power.open;
            }
            PanelKind::Calendar => {
                self.calendar.open = !self.calendar.open;
                opened = self.calendar.open;
            }
            _ => {}
        }

        if opened {
            self.owner_output = Some(owner_output);
        } else if !self.has_open_panels() {
            self.owner_output = None;
        }
    }

    /// Run egui panel logic and collect [`UiAction`]s (without painting).
    pub fn update_panels(&mut self, frame_ctx: &DesktopFrameCtx) {
        self.last_frame_ctx = Some(frame_ctx.clone());
        self.prepare_raw_input(frame_ctx);

        let output = self.ctx.run(self.raw_input.take(), |ctx| {
            self.settings.show(ctx, frame_ctx, &mut self.actions);
            self.launcher.show(ctx, frame_ctx, &mut self.actions);
            self.network.show(ctx, frame_ctx, &mut self.actions);
            self.bluetooth.show(ctx, frame_ctx, &mut self.actions);
            self.audio.show(ctx, frame_ctx, &mut self.actions);
            self.power.show(ctx, frame_ctx, &mut self.actions);
            self.calendar.show(ctx, frame_ctx, &mut self.actions);
            self.debug.show(ctx, frame_ctx, &mut self.actions);
        });

        self.textures_delta.append(output.textures_delta);
        self.primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        self.wants_pointer_input = self.ctx.wants_pointer_input() || self.ctx.is_using_pointer();
        self.wants_keyboard_input = self.ctx.wants_keyboard_input();
    }

    fn run_panels(&mut self, frame_ctx: &DesktopFrameCtx) {
        self.update_panels(frame_ctx);
    }

    pub fn handle_input(&mut self, event: EguiInputEvent) -> bool {
        match event {
            EguiInputEvent::PointerMoved { position } => {
                let pos = Pos2::new(position.x as f32, position.y as f32);
                self.last_pointer_pos = Some(pos);
                self.raw_input.events.push(Event::PointerMoved(pos));
            }
            EguiInputEvent::PointerButton {
                button,
                pressed,
                position,
                modifiers,
            } => {
                let pos = Pos2::new(position.x as f32, position.y as f32);
                self.last_pointer_pos = Some(pos);
                self.raw_input.events.push(Event::PointerButton {
                    pos,
                    button: pointer_button(button),
                    pressed,
                    modifiers: modifiers.into(),
                });
            }
            EguiInputEvent::PointerScroll {
                delta, modifiers, ..
            } => {
                let (unit, delta) = match delta {
                    EguiScrollDelta::Line { x, y } => (MouseWheelUnit::Line, Vec2::new(x, y)),
                    EguiScrollDelta::Point { x, y } => (MouseWheelUnit::Point, Vec2::new(x, y)),
                };
                self.raw_input.events.push(Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: modifiers.into(),
                });
            }
            EguiInputEvent::PointerGone => {
                self.raw_input.events.push(Event::PointerGone);
            }
            EguiInputEvent::Key {
                key,
                pressed,
                repeat,
                modifiers,
            } => {
                if let Some(key) = key {
                    self.raw_input.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat,
                        modifiers: modifiers.into(),
                    });
                }
            }
        }

        self.wants_pointer_input || self.wants_keyboard_input
    }

    pub fn close_all_panels(&mut self) {
        self.settings.open = false;
        self.launcher.open = false;
        self.network.open = false;
        self.bluetooth.open = false;
        self.audio.open = false;
        self.power.open = false;
        self.calendar.open = false;
        self.debug.open = false;
        self.owner_output = None;
    }

    pub fn wants_pointer_input(&self) -> bool {
        self.wants_pointer_input
    }

    pub fn wants_keyboard_input(&self) -> bool {
        self.wants_keyboard_input
    }

    pub fn take_actions(&mut self) -> Vec<UiAction> {
        mem::take(&mut self.actions)
    }

    pub fn clear_paint(&mut self) {
        self.primitives.clear();
        self.wants_pointer_input = false;
        self.wants_keyboard_input = false;
    }

    pub fn render(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        damage: &[Rectangle<i32, Physical>],
        shaders: &ChromeShaders,
        theme: &FlowTheme,
    ) -> Result<(), GlesError> {
        if !self.has_open_panels() {
            self.clear_paint();
            return Ok(());
        }

        if self.owner_output != Some(frame_ctx.rendering_output) {
            return Ok(());
        }

        // Panel logic runs in [`DesktopState::sync_egui`] before paint so input events
        // are not consumed mid-click by a separate ctx.run during render.

        let mut bridge = EguiShaderBridge {
            frame,
            frame_ctx,
            damage,
            shaders,
            theme,
        };
        self.paint_shader_effects(&mut bridge)?;
        self.render_egui_gl(frame, frame_ctx)
    }

    pub fn render_with_shader_bridge(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
        damage: &[Rectangle<i32, Physical>],
        shaders: &ChromeShaders,
        theme: &FlowTheme,
        paint: impl FnOnce(&mut EguiShaderBridge<'_, '_, '_>) -> Result<(), GlesError>,
    ) -> Result<(), GlesError> {
        let mut bridge = EguiShaderBridge {
            frame,
            frame_ctx,
            damage,
            shaders,
            theme,
        };
        paint(&mut bridge)
    }

    pub fn run_ui(
        &mut self,
        frame_ctx: &DesktopFrameCtx,
        build: impl FnOnce(&Context, &mut Vec<UiAction>),
    ) {
        self.prepare_raw_input(frame_ctx);
        let mut actions = Vec::new();
        let mut build = Some(build);
        let output = self.ctx.run(self.raw_input.take(), |ctx| {
            if let Some(build) = build.take() {
                build(ctx, &mut actions);
            }
        });
        self.textures_delta.append(output.textures_delta);
        self.primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        self.actions.extend(actions);
        self.wants_pointer_input = self.ctx.wants_pointer_input() || self.ctx.is_using_pointer();
        self.wants_keyboard_input = self.ctx.wants_keyboard_input();
    }

    /*
    fn run_default_overlay(&mut self, frame_ctx: &DesktopFrameCtx) {
        let demo_open = self.demo_open;
        self.run_ui(frame_ctx, |ctx, actions| {
            if !demo_open {
                return;
            }
            let work = egui_work_rect(frame_ctx);

            egui::Area::new("focaldesk_egui_overlay".into())
                .fixed_pos(work.min + egui::vec2(16.0, 16.0))
                .show(ctx, |ui| {
                    ui.set_min_width(220.0);
                    ui.heading("FocalDesk");
                    ui.separator();
                    if ui.button("Open settings panel").clicked() {
                        actions.push(UiAction::OpenPanel(PanelKind::Settings));
                    }
                    if ui.button("Launch terminal").clicked() {
                        actions.push(UiAction::LaunchApp("weston-terminal"));
                    }
                    if ui.button("Custom action").clicked() {
                        actions.push(UiAction::Custom(9001));
                    }
                });
        });
    }
    */

    fn paint_shader_effects(
        &self,
        bridge: &mut EguiShaderBridge<'_, '_, '_>,
    ) -> Result<(), GlesError> {
        let _ = bridge;
        Ok(())
    }

    fn prepare_raw_input(&mut self, frame_ctx: &DesktopFrameCtx) {
        self.screen_height_pts = (frame_ctx.output_size.1 as f32) / frame_ctx.output_scale.y as f32;
        self.raw_input
            .viewports
            .entry(self.raw_input.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(frame_ctx.output_scale.x as f32);
        self.raw_input.screen_rect = Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(
                (frame_ctx.output_size.0 as f64 / frame_ctx.output_scale.x) as f32,
                self.screen_height_pts,
            ),
        ));
        self.raw_input.time = Some(
            frame_ctx
                .now
                .duration_since(frame_ctx.start_time)
                .as_secs_f64(),
        );
        self.raw_input.predicted_dt = 1.0 / 60.0;
    }

    fn render_egui_gl(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        frame_ctx: &DesktopFrameCtx,
    ) -> Result<(), GlesError> {
        if egui_debug_uv_enabled() {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                flog_info!(
                    "egui: FOCALDESK_EGUI_DEBUG_UV is ignored when using the egui_glow painter"
                );
            });
        }

        self.log_debug_dumps();

        let screen_size_px = [
            frame_ctx.output_size.0 as u32,
            frame_ctx.output_size.1 as u32,
        ];
        let pixels_per_point = frame_ctx.output_scale.x as f32;

        frame.with_context(|_gl| {
            if let Err(err) = self.paint_with_glow(frame_ctx, screen_size_px, pixels_per_point) {
                flog_error!("egui glow paint failed: {err}");
            }
        })
    }

    fn log_debug_dumps(&mut self) {
        for (id, delta) in &self.textures_delta.set {
            let [w, h] = delta.image.size();
            if w == 0 || h == 0 {
                continue;
            }

            if !self.logged_texture_delta {
                let stats = image_alpha_stats(&delta.image);
                flog_info!(
                    "egui texture delta: id={id:?} kind={} pos={:?} size={}x{} alpha_nonzero={} alpha_min={} alpha_max={}",
                    image_kind(&delta.image),
                    delta.pos,
                    w,
                    h,
                    stats.nonzero,
                    stats.min,
                    stats.max
                );
                self.logged_texture_delta = true;
            }

            if !self.dumped_font_atlas && matches!(delta.image, ImageData::Font(_)) {
                let rgba = image_delta_rgba_raw(&delta.image);
                if let Err(err) =
                    dump_egui_atlas_png("/tmp/focaldesk-egui-font-atlas.png", &rgba, [w, h])
                {
                    flog_error!("egui texture warning: failed to dump font atlas: {err}");
                } else {
                    flog_info!("egui texture dump: /tmp/focaldesk-egui-font-atlas.png");
                }
                self.dumped_font_atlas = true;
            }

            if matches!(delta.image, ImageData::Font(_)) && delta.pos.is_none() {
                self.last_font_atlas_rgba = Some(image_delta_rgba_raw(&delta.image));
            }
        }

        for clipped in &self.primitives {
            let Primitive::Mesh(mesh) = &clipped.primitive else {
                continue;
            };
            if !self.logged_mesh_sample && mesh.texture_id == TextureId::default() {
                let uv = mesh_uv_bounds(&mesh.vertices);
                if mesh_has_atlas_uvs(uv) {
                    let atlas_size = self
                        .textures_delta
                        .set
                        .iter()
                        .find(|(tid, _)| *tid == TextureId::default())
                        .map(|(_, d)| d.image.size())
                        .unwrap_or([0, 0]);
                    flog_info!(
                        "egui text mesh sample: texture={:?} texture_size={:?} vertices={} indices={} uv_min={:?} uv_max={:?}",
                        mesh.texture_id,
                        atlas_size,
                        mesh.vertices.len(),
                        mesh.indices.len(),
                        uv.0,
                        uv.1
                    );
                    self.logged_mesh_sample = true;
                }
            }
            if self.dumped_font_mesh || mesh.texture_id != TextureId::default() {
                continue;
            }
            let uv = mesh_uv_bounds(&mesh.vertices);
            if !mesh_has_atlas_uvs(uv) {
                continue;
            }
            let Some(font_rgba) = self.last_font_atlas_rgba.as_ref() else {
                continue;
            };
            let [atlas_w, atlas_h] = self
                .textures_delta
                .set
                .iter()
                .find(|(tid, _)| *tid == TextureId::default())
                .map(|(_, d)| d.image.size())
                .unwrap_or([0, 0]);
            if atlas_w == 0 || atlas_h == 0 {
                continue;
            }
            match dump_egui_mesh_png(
                "/tmp/focaldesk-egui-font-mesh.png",
                font_rgba,
                [atlas_w, atlas_h],
                &mesh.vertices,
                &mesh.indices,
            ) {
                Ok(()) => flog_info!("egui mesh dump: /tmp/focaldesk-egui-font-mesh.png"),
                Err(err) => flog_error!("egui mesh warning: failed to dump font mesh: {err}"),
            }
            self.dumped_font_mesh = true;
        }
    }

    fn paint_with_glow(
        &mut self,
        frame_ctx: &DesktopFrameCtx,
        screen_size_px: [u32; 2],
        pixels_per_point: f32,
    ) -> Result<(), egui_glow::PainterError> {
        if self.glow_painter.is_none() {
            let gl = Arc::new(unsafe {
                glow::Context::from_loader_function(|symbol| get_proc_address(symbol) as *const _)
            });
            let painter = Painter::new(gl, "", None, false)?;
            flog_info!("egui: using egui_glow painter");
            self.glow_painter = Some(painter);
        }

        let height_pts = screen_size_px[1] as f32 / pixels_per_point;
        let primitives = if frame_ctx.flip_egui_y {
            flip_clipped_primitives_y(&self.primitives, height_pts)
        } else {
            self.primitives.clone()
        };

        let painter = self.glow_painter.as_mut().expect("initialized above");
        painter.paint_and_update_textures(
            screen_size_px,
            pixels_per_point,
            &primitives,
            &self.textures_delta,
        );
        self.textures_delta.set.clear();
        self.textures_delta.free.clear();
        Ok(())
    }
}

impl Drop for EguiLayer {
    fn drop(&mut self) {
        if let Some(mut painter) = self.glow_painter.take() {
            painter.destroy();
        }
    }
}

fn pointer_button(button: EguiPointerButton) -> PointerButton {
    match button {
        EguiPointerButton::Primary => PointerButton::Primary,
        EguiPointerButton::Secondary => PointerButton::Secondary,
        EguiPointerButton::Middle => PointerButton::Middle,
        EguiPointerButton::Extra1 => PointerButton::Extra1,
        EguiPointerButton::Extra2 => PointerButton::Extra2,
    }
}

impl From<EguiModifiers> for Modifiers {
    fn from(value: EguiModifiers) -> Self {
        Self {
            alt: value.alt,
            ctrl: value.ctrl,
            shift: value.shift,
            mac_cmd: value.mac_cmd,
            command: value.command,
        }
    }
}

fn image_delta_rgba_raw(image: &ImageData) -> Vec<u8> {
    match image {
        ImageData::Color(image) => image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect(),
        ImageData::Font(image) => image
            .srgba_pixels(None)
            .flat_map(|pixel| pixel.to_array())
            .collect(),
    }
}

fn image_kind(image: &ImageData) -> &'static str {
    match image {
        ImageData::Color(_) => "color",
        ImageData::Font(_) => "font",
    }
}

#[derive(Debug, Clone, Copy)]
struct AlphaStats {
    nonzero: usize,
    min: u8,
    max: u8,
}

fn image_alpha_stats(image: &ImageData) -> AlphaStats {
    let mut stats = AlphaStats {
        nonzero: 0,
        min: u8::MAX,
        max: 0,
    };

    match image {
        ImageData::Color(image) => {
            for pixel in &image.pixels {
                let alpha = pixel.a();
                stats.nonzero += usize::from(alpha != 0);
                stats.min = stats.min.min(alpha);
                stats.max = stats.max.max(alpha);
            }
        }
        ImageData::Font(image) => {
            for coverage in &image.pixels {
                let alpha = (coverage.powf(0.55) * 255.0).round().clamp(0.0, 255.0) as u8;
                stats.nonzero += usize::from(alpha != 0);
                stats.min = stats.min.min(alpha);
                stats.max = stats.max.max(alpha);
            }
        }
    }

    if stats.min == u8::MAX {
        stats.min = 0;
    }
    stats
}

fn mesh_uv_bounds(vertices: &[egui::epaint::Vertex]) -> ([f32; 2], [f32; 2]) {
    let mut min = [f32::INFINITY, f32::INFINITY];
    let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];

    for vertex in vertices {
        min[0] = min[0].min(vertex.uv.x);
        min[1] = min[1].min(vertex.uv.y);
        max[0] = max[0].max(vertex.uv.x);
        max[1] = max[1].max(vertex.uv.y);
    }

    if vertices.is_empty() {
        ([0.0, 0.0], [0.0, 0.0])
    } else {
        (min, max)
    }
}

fn mesh_has_atlas_uvs((min, max): ([f32; 2], [f32; 2])) -> bool {
    let span_x = max[0] - min[0];
    let span_y = max[1] - min[1];
    min[0].is_finite()
        && min[1].is_finite()
        && max[0].is_finite()
        && max[1].is_finite()
        && (span_x > 0.0001 || span_y > 0.0001)
        && (max[0] > 0.001 || max[1] > 0.001)
}

fn egui_debug_uv_enabled() -> bool {
    std::env::var_os("FOCALDESK_EGUI_DEBUG_UV").is_some()
}

fn flip_clipped_primitives_y(
    primitives: &[ClippedPrimitive],
    height_pts: f32,
) -> Vec<ClippedPrimitive> {
    primitives
        .iter()
        .map(|clipped| {
            let mut flipped = clipped.clone();
            flipped.clip_rect = Rect::from_min_max(
                Pos2::new(
                    clipped.clip_rect.min.x,
                    height_pts - clipped.clip_rect.max.y,
                ),
                Pos2::new(
                    clipped.clip_rect.max.x,
                    height_pts - clipped.clip_rect.min.y,
                ),
            );
            if let Primitive::Mesh(mesh) = &mut flipped.primitive {
                for vertex in &mut mesh.vertices {
                    vertex.pos.y = height_pts - vertex.pos.y;
                }
            }
            flipped
        })
        .collect()
}

fn dump_egui_atlas_png(
    path: &str,
    rgba: &[u8],
    [w, h]: [usize; 2],
) -> Result<(), image::ImageError> {
    let Some(image) = image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec()) else {
        return Err(image::ImageError::Parameter(
            image::error::ParameterError::from_kind(
                image::error::ParameterErrorKind::DimensionMismatch,
            ),
        ));
    };
    image.save(path)
}

fn dump_egui_mesh_png(
    path: &str,
    atlas_rgba: &[u8],
    [atlas_w, atlas_h]: [usize; 2],
    vertices: &[egui::epaint::Vertex],
    indices: &[u32],
) -> Result<(), image::ImageError> {
    if vertices.is_empty() || indices.is_empty() || atlas_w == 0 || atlas_h == 0 {
        return Ok(());
    }

    let mut min = [f32::INFINITY, f32::INFINITY];
    let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
    for vertex in vertices {
        min[0] = min[0].min(vertex.pos.x);
        min[1] = min[1].min(vertex.pos.y);
        max[0] = max[0].max(vertex.pos.x);
        max[1] = max[1].max(vertex.pos.y);
    }

    let margin = 8.0;
    let width = ((max[0] - min[0] + margin * 2.0).ceil() as u32).clamp(1, 2048);
    let height = ((max[1] - min[1] + margin * 2.0).ceil() as u32).clamp(1, 2048);
    let origin = [min[0] - margin, min[1] - margin];
    let mut out = image::RgbaImage::new(width, height);

    for triangle in indices.chunks_exact(3) {
        let [Some(a), Some(b), Some(c)] = [
            vertices.get(triangle[0] as usize),
            vertices.get(triangle[1] as usize),
            vertices.get(triangle[2] as usize),
        ] else {
            continue;
        };

        let ax = a.pos.x - origin[0];
        let ay = a.pos.y - origin[1];
        let bx = b.pos.x - origin[0];
        let by = b.pos.y - origin[1];
        let cx = c.pos.x - origin[0];
        let cy = c.pos.y - origin[1];

        let x0 = ax.min(bx).min(cx).floor().max(0.0) as u32;
        let y0 = ay.min(by).min(cy).floor().max(0.0) as u32;
        let x1 = ax.max(bx).max(cx).ceil().min(width as f32 - 1.0) as u32;
        let y1 = ay.max(by).max(cy).ceil().min(height as f32 - 1.0) as u32;
        let denom = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy);
        if denom.abs() < f32::EPSILON {
            continue;
        }

        for y in y0..=y1 {
            for x in x0..=x1 {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let wa = ((by - cy) * (px - cx) + (cx - bx) * (py - cy)) / denom;
                let wb = ((cy - ay) * (px - cx) + (ax - cx) * (py - cy)) / denom;
                let wc = 1.0 - wa - wb;
                if wa < -0.001 || wb < -0.001 || wc < -0.001 {
                    continue;
                }

                let u = wa * a.uv.x + wb * b.uv.x + wc * c.uv.x;
                let v = wa * a.uv.y + wb * b.uv.y + wc * c.uv.y;
                let tx = ((u * atlas_w as f32).floor() as isize).clamp(0, atlas_w as isize - 1);
                let ty = ((v * atlas_h as f32).floor() as isize).clamp(0, atlas_h as isize - 1);
                let atlas_idx = ((ty as usize * atlas_w + tx as usize) * 4)
                    .min(atlas_rgba.len().saturating_sub(4));
                let src = &atlas_rgba[atlas_idx..atlas_idx + 4];
                let color = a.color.to_array();
                let alpha = ((src[3] as u16 * color[3] as u16) / 255) as u8;
                if alpha == 0 {
                    continue;
                }
                out.put_pixel(
                    x,
                    y,
                    image::Rgba([
                        ((src[0] as u16 * color[0] as u16) / 255) as u8,
                        ((src[1] as u16 * color[1] as u16) / 255) as u8,
                        ((src[2] as u16 * color[2] as u16) / 255) as u8,
                        alpha,
                    ]),
                );
            }
        }
    }

    out.save(path)
}

fn egui_work_rect(frame_ctx: &DesktopFrameCtx) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(frame_ctx.work.loc.x as f32, frame_ctx.work.loc.y as f32),
        egui::vec2(frame_ctx.work.size.w as f32, frame_ctx.work.size.h as f32),
    )
}

#[cfg(test)]
mod egui_vertex_layout_tests {
    use egui::epaint::Vertex;
    use std::mem;

    #[test]
    fn vertex_layout_matches_gl_attribs() {
        assert_eq!(mem::size_of::<Vertex>(), 20);
        assert_eq!(mem::offset_of!(Vertex, pos), 0);
        assert_eq!(mem::offset_of!(Vertex, uv), 8);
        assert_eq!(mem::offset_of!(Vertex, color), 16);
    }
}
