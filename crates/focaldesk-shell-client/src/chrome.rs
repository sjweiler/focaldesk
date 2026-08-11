use anyhow::{anyhow, Result};

use smithay::backend::renderer::gles::{ffi, GlesError, GlesFrame, GlesTexture, Uniform};
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::{ImportMem, Renderer, RendererSuper, Texture};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};
//use crate::icons::{IconCache, IconId, IconKey, IconState};
use crate::atlas::{
    build_icon_atlas, render_atlas_icon, render_atlas_icon_with_alpha, IconAtlas, IconId,
};
use crate::chrome_draw::{draw_beveled_panel, draw_recessed_button, draw_top_bar, well_icon_rect};
use crate::chrome_layout::{build_chrome_layout_with_config, ChromeLayoutConfig};
use crate::chrome_shaders::ChromeShaders;
use crate::chrome_theme::{chrome_theme_from_flow_theme, ChromeTheme};
use crate::controls::ShellControl;
use crate::font_atlas::FontAtlas;
use crate::svg::rasterize_svg;
use focaldesk_themes::FlowTheme;
use image::GenericImageView;
use smithay::backend::allocator::Fourcc;

#[derive(Debug, Clone, Copy)]
pub struct PulseFrame {
    pub control: usize,
    pub click: (f64, f64),
    pub elapsed: std::time::Duration,
}

pub fn load_svg_texture<R>(
    renderer: &mut R,
    svg_bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<GlesTexture>
where
    R: Renderer<TextureId = GlesTexture> + ImportMem,
    <R as RendererSuper>::Error: std::fmt::Debug,
{
    let rgba = rasterize_svg(svg_bytes, width, height)?;

    let size = Size::from((rgba.width() as i32, rgba.height() as i32));

    let tex = renderer
        .import_memory(rgba.as_raw(), Fourcc::Abgr8888, size, false)
        .map_err(|e| anyhow!("import_memory failed for SVG texture: {:?}", e))?;

    Ok(tex)
}

pub fn load_png_texture<R>(renderer: &mut R, path: &str) -> Result<GlesTexture>
where
    R: Renderer<TextureId = GlesTexture> + ImportMem,
    <R as RendererSuper>::Error: std::fmt::Debug,
{
    let img = image::open(path)?;
    let rgba = img.to_rgba8();
    let (width, height) = img.dimensions();

    let size = Size::from((width as i32, height as i32));

    let tex = renderer
        .import_memory(rgba.as_raw(), Fourcc::Abgr8888, size, false)
        .map_err(|e| anyhow!("import_memory failed for {path}: {:?}", e))?;

    Ok(tex)
}

pub(crate) fn dock_slot_rects(
    logical_w: i32,
    logical_h: i32,
    requested: usize,
) -> Vec<Rectangle<i32, Logical>> {
    let slot_w = (logical_w - 16).max(16);
    let max_slots = ((logical_h - 10 - 24 + 8).max(0) / 56) as usize;
    (0..requested.min(max_slots))
        .map(|index| Rectangle::from_loc_and_size((8, 10 + index as i32 * 56), (slot_w, 48)))
        .collect()
}

fn scale_rect_about_center(rect: Rectangle<i32, Logical>, factor: f64) -> Rectangle<i32, Logical> {
    let w = (rect.size.w as f64 * factor).round() as i32;
    let h = (rect.size.h as f64 * factor).round() as i32;
    Rectangle::from_loc_and_size(
        (
            rect.loc.x - (w - rect.size.w) / 2,
            rect.loc.y - (h - rect.size.h) / 2,
        ),
        (w.max(1), h.max(1)),
    )
}

fn centered_square(rect: Rectangle<i32, Logical>, side: i32) -> Rectangle<i32, Logical> {
    let side = side.min(rect.size.w).min(rect.size.h).max(1);
    Rectangle::from_loc_and_size(
        (
            rect.loc.x + (rect.size.w - side) / 2,
            rect.loc.y + (rect.size.h - side) / 2,
        ),
        (side, side),
    )
}

#[derive(Default, Debug, Clone)]
pub struct ClockCache {
    pub last_string: String,
    pub texture: Option<GlesTexture>,
    pub scale: f64,
}

impl ClockCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Simple chrome config (tune to your design).
#[derive(Debug, Clone)]
pub struct ChromeMetrics {
    pub sidebar_w: i32,
    pub topbar_h: i32,
    pub icon_base_px: u32, // design icon size (24 recommended)
    pub icon_padding: i32, // padding from edges
    pub slot_spacing: i32, // vertical spacing between slot icons
}

impl Default for ChromeMetrics {
    fn default() -> Self {
        Self {
            sidebar_w: 76,
            topbar_h: 64,
            icon_base_px: 36,
            icon_padding: 10,
            slot_spacing: 18,
        }
    }
}

/// Chrome state: owns cached textures.
pub struct Chrome {
    pub metrics: ChromeMetrics,
    //icon_cache: Option<IconCache<GlesTexture>>,
    pub topbar_tex: Option<GlesTexture>,
    pub sidebar_tex: Option<GlesTexture>,
    pub atlas: Option<IconAtlas>,
    pub font_atlas: Option<FontAtlas>,
    pub shaders: ChromeShaders,
    pub theme: ChromeTheme,
    glass_background: Option<GlesTexture>,
}

impl Chrome {
    pub fn new(metrics: ChromeMetrics) -> Self {
        Self {
            metrics,
            //icon_cache: None,
            topbar_tex: None,
            sidebar_tex: None,
            atlas: None,
            font_atlas: None,
            shaders: ChromeShaders::new(),
            theme: chrome_theme_from_flow_theme(&FlowTheme::default().chrome),
            glass_background: None,
        }
    }

    /// Drop the cached icon atlas and any other GPU texture handles so
    /// `ensure_gpu_resources` rebuilds them from scratch.
    ///
    /// Needed after resuming from suspend: the DRM backend recreates the
    /// `EGLContext`, which invalidates the `GlesTexture` handles inside
    /// `atlas`/`topbar_tex`/`sidebar_tex`. Without this, `ensure_gpu_resources`
    /// sees `atlas.is_some()` and never rebuilds it, leaving sidebar/topbar
    /// icons blank forever.
    pub fn invalidate_gpu_state(&mut self) {
        self.atlas = None;
        self.font_atlas = None;
        self.shaders = ChromeShaders::new();
        self.glass_background = None;
        self.topbar_tex = None;
        self.sidebar_tex = None;
    }

    /// Ensure icon textures exist for this scale.
    /// Call from App::render() before drawing.
    pub fn ensure_gpu_resources<R>(&mut self, renderer: &mut R, _scale: f64) -> Result<()>
    where
        R: Renderer<TextureId = GlesTexture> + ImportMem,
    {
        if self.atlas.is_none() {
            self.atlas = Some(build_icon_atlas(renderer)?);
        }

        //if self.topbar_tex.is_none() {
        //    self.topbar_tex = Some(load_png_texture(renderer, "assets/topbar_frame.png")?);
        // }

        // if self.sidebar_tex.is_none() {
        //     self.sidebar_tex = Some(load_png_texture(renderer, "assets/sidebar_frame.png")?);
        //  }

        Ok(())
    }

    pub fn ensure_font_resources(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    ) -> Result<()> {
        if self.font_atlas.is_none() {
            self.font_atlas = Some(FontAtlas::new()?);
        }
        self.font_atlas
            .as_mut()
            .unwrap()
            .ensure_uploaded(renderer)
            .map_err(|e| anyhow!("upload IBM Plex font atlas: {e:?}"))
    }

    pub fn ensure_shader_resources(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    ) -> Result<()> {
        self.shaders
            .ensure_shell_compiled(renderer)
            .map_err(|e| anyhow!("compile shell chrome shaders: {e:?}"))
            .and_then(|_| {
                if self.shaders.beveled_panel.is_none()
                    || self.shaders.recessed_button.is_none()
                    || self.shaders.top_bar.is_none()
                {
                    return Err(anyhow!(
                        "one or more required shell chrome shaders failed to compile"
                    ));
                }
                if self.glass_background.is_none() {
                    self.glass_background = Some(renderer.import_memory(
                        &[22, 28, 42, 255],
                        smithay::backend::allocator::Fourcc::Abgr8888,
                        (1, 1).into(),
                        false,
                    )?);
                }
                Ok(())
            })
    }

    pub fn render_text(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        text: &str,
        x: i32,
        baseline_y: i32,
        output_size: Size<i32, Physical>,
        scale: f64,
        tint: [f32; 4],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        if let Some(font_atlas) = &self.font_atlas {
            font_atlas.render(frame, text, x, baseline_y, output_size, scale, tint)?;
        }
        Ok(())
    }

    /// Draw the chrome. No renderer required — only a Frame.
    pub fn render_chrome(
        &self,
        frame: &mut impl Frame<
            TextureId = GlesTexture,
            Error = smithay::backend::renderer::gles::GlesError,
        >,
        output_size: Size<i32, Physical>,
    ) {
        // Sidebar & topbar rectangles are placeholders.
        // Replace these with your own rect-fill helpers if you already have them.
        let _sidebar_rect: Rectangle<i32, Physical> =
            Rectangle::from_loc_and_size((0, 0), (self.metrics.sidebar_w, output_size.h));

        let _topbar_rect: Rectangle<i32, Physical> =
            Rectangle::from_loc_and_size((0, 0), (output_size.w, self.metrics.topbar_h));

        // Draw icons (example positions).
        self.draw_topbar_icons(frame, output_size);
        self.draw_sidebar_slots(frame, output_size);
    }

    /// Render only the top-bar portion for a standalone layer-shell panel.
    pub fn render_panel(
        &self,
        frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
        output_size: Size<i32, Physical>,
        scale: f64,
        controls: &[ShellControl],
        hovered: Option<usize>,
        pulse: Option<PulseFrame>,
    ) -> std::result::Result<(), GlesError> {
        let beveled = self.shaders.beveled_panel.as_ref();
        let button = self.shaders.recessed_button.as_ref();
        let damage = [Rectangle::from_size(output_size)];
        let sc = Scale::from((scale, scale));
        let logical: Size<i32, Logical> = Size::from((
            (output_size.w as f64 / scale) as i32,
            (output_size.h as f64 / scale) as i32,
        ));
        let layout = build_chrome_layout_with_config(
            logical,
            self.metrics.topbar_h,
            self.metrics.sidebar_w,
            ChromeLayoutConfig {
                status_item_count: controls.len(),
                sidebar_item_count: 0,
            },
        );
        if let Some(top_bar) = self.shaders.top_bar.as_ref() {
            draw_top_bar(
                frame,
                top_bar,
                layout.topbar.outer,
                sc,
                &damage,
                &self.theme.top_bar,
            )?;
        }
        if let Some(beveled) = beveled {
            for (rect, style) in [
                (layout.topbar.inner, &self.theme.frame_inner),
                (layout.topbar.title, &self.theme.panel_inner),
                (layout.topbar.trim, &self.theme.trim),
            ] {
                draw_beveled_panel(frame, beveled, rect, sc, &damage, style)?;
            }
        }
        let launcher_control = layout.topbar.flow_field;
        if let Some(button) = button {
            let mut style = self.theme.button;
            if hovered == Some(0) {
                style.glow_strength = 0.72;
                style.glow_radius = style.glow_radius.max(7.0);
            }
            draw_recessed_button(frame, button, launcher_control, sc, &damage, &style)?;
        }
        // Match the compositor's TopbarFlowField constructor: keep the AI
        // glyph square and cap it at 28 logical pixels inside the wide well.
        let mut launcher_icon = centered_square(launcher_control, launcher_control.size.h.min(28));
        if hovered == Some(0) {
            launcher_icon = scale_rect_about_center(launcher_icon, 1.08);
        }
        let launcher_state = ShellControl {
            icon: IconId::AiConsole,
            tooltip: "Launch FocalDesk AI Console".into(),
            action: focaldesk_ipc::DesktopAction::LaunchApp {
                app: "focaldesk-ai-console".into(),
            },
            selected: false,
            active: false,
            enabled: true,
        };
        if self.shaders.glass_control.is_some() && self.glass_background.is_some() {
            self.draw_glass_icon(
                frame,
                IconId::AiConsole,
                launcher_control,
                launcher_icon,
                scale,
                output_size,
                hovered == Some(0),
                &launcher_state,
            )?;
        }
        self.render_icon(frame, IconId::AiConsole, launcher_icon, sc, output_size)?;
        if let Some(pulse) = pulse.filter(|pulse| pulse.control == 0) {
            self.draw_pulse(frame, launcher_control, pulse, sc, &damage)?;
        }

        for (index, (well, control)) in layout.topbar.status_wells.iter().zip(controls).enumerate()
        {
            let control_index = index + 1;
            if let Some(button) = button {
                let mut style = self.theme.button;
                if control.selected || control.active || hovered == Some(control_index) {
                    style.glow_strength = if control.active { 1.0 } else { 0.72 };
                    style.glow_radius = style.glow_radius.max(7.0);
                }
                if !control.enabled {
                    style.face_color[3] *= 0.52;
                    style.glow_strength = 0.0;
                }
                draw_recessed_button(frame, button, *well, sc, &damage, &style)?;
            }
            let mut icon_rect = well_icon_rect(*well);
            if hovered == Some(control_index) {
                icon_rect = scale_rect_about_center(icon_rect, 1.08);
            }
            if self.shaders.glass_control.is_some() && self.glass_background.is_some() {
                self.draw_glass_icon(
                    frame,
                    control.icon,
                    *well,
                    icon_rect,
                    scale,
                    output_size,
                    hovered == Some(control_index),
                    control,
                )?;
            }
            self.render_icon_alpha(
                frame,
                control.icon,
                icon_rect,
                sc,
                output_size,
                if control.enabled { 1.0 } else { 0.38 },
            )?;
            if let Some(pulse) = pulse.filter(|pulse| pulse.control == control_index) {
                self.draw_pulse(frame, *well, pulse, sc, &damage)?;
            }
        }
        let clock_index = controls.len() + 1;
        if let Some(button) = button {
            let mut style = self.theme.button;
            if hovered == Some(clock_index) {
                style.glow_strength = 0.72;
                style.glow_radius = style.glow_radius.max(7.0);
            }
            draw_recessed_button(frame, button, layout.topbar.clock_well, sc, &damage, &style)?;
        }
        if let Some(pulse) = pulse.filter(|pulse| pulse.control == clock_index) {
            self.draw_pulse(frame, layout.topbar.clock_well, pulse, sc, &damage)?;
        }
        if let Some(index) = hovered {
            let (tooltip, anchor) = if index == 0 {
                ("Launch FocalDesk AI Console", launcher_control)
            } else if index == clock_index {
                ("Calendar and clock", layout.topbar.clock_well)
            } else if let Some(control) = controls.get(index - 1) {
                (
                    control.tooltip.as_str(),
                    layout.topbar.status_wells[index - 1],
                )
            } else {
                ("", launcher_control)
            };
            if !tooltip.is_empty() {
                self.render_tooltip(frame, tooltip, anchor, output_size, scale, true)?;
            }
        }
        Ok(())
    }

    /// Render only the sidebar portion for a standalone layer-shell dock.
    pub fn render_dock(
        &self,
        frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
        output_size: Size<i32, Physical>,
        controls: &[ShellControl],
        hovered: Option<usize>,
        pulse: Option<PulseFrame>,
        scale: f64,
    ) -> std::result::Result<(), GlesError> {
        let beveled = self.shaders.beveled_panel.as_ref();
        let button = self.shaders.recessed_button.as_ref();
        let damage = [Rectangle::from_size(output_size)];
        let sc = Scale::from((scale, scale));
        let logical_w = self.metrics.sidebar_w;
        let logical_h = (output_size.h as f64 / scale) as i32;
        let outer = Rectangle::<i32, Logical>::from_loc_and_size(
            (0, 0),
            (logical_w.max(1), logical_h.max(1)),
        );
        let inner = crate::chrome_draw::inset_rect(outer, 4);
        if let Some(beveled) = beveled {
            draw_beveled_panel(frame, beveled, outer, sc, &damage, &self.theme.sidebar)?;
            draw_beveled_panel(frame, beveled, inner, sc, &damage, &self.theme.panel_inner)?;
        }

        let slots = dock_slot_rects(logical_w, logical_h, controls.len());
        for (index, (control, slot_outer)) in controls.iter().zip(slots).enumerate() {
            let slot_inner = crate::chrome_draw::inset_rect(slot_outer, 2);
            let icon_well = crate::chrome_draw::inset_rect(slot_inner, 3);
            let active = control.selected || control.active;
            if let Some(beveled) = beveled {
                draw_beveled_panel(frame, beveled, slot_outer, sc, &damage, &self.theme.module)?;
                draw_beveled_panel(
                    frame,
                    beveled,
                    slot_inner,
                    sc,
                    &damage,
                    if active {
                        &self.theme.icon_well_active
                    } else {
                        &self.theme.module_inner
                    },
                )?;
            }
            if let Some(button) = button {
                let mut style = self.theme.button;
                if active || hovered == Some(index) {
                    style.glow_strength = 1.0;
                    style.glow_radius = style.glow_radius.max(7.0);
                    style.face_color = self.theme.icon_well_active.face_color;
                }
                if !control.enabled {
                    style.face_color[3] *= 0.52;
                    style.glow_strength = 0.0;
                }
                draw_recessed_button(frame, button, icon_well, sc, &damage, &style)?;
            }
            let mut icon_rect = well_icon_rect(icon_well);
            if hovered == Some(index) {
                icon_rect = scale_rect_about_center(icon_rect, 1.10);
            }
            if self.shaders.glass_control.is_some() && self.glass_background.is_some() {
                self.draw_glass_icon(
                    frame,
                    control.icon,
                    icon_well,
                    icon_rect,
                    scale,
                    output_size,
                    hovered == Some(index),
                    control,
                )?;
            }
            self.render_icon_alpha(
                frame,
                control.icon,
                icon_rect,
                sc,
                output_size,
                if control.enabled { 1.0 } else { 0.38 },
            )?;
            if let Some(pulse) = pulse.filter(|pulse| pulse.control == index) {
                self.draw_pulse(frame, slot_outer, pulse, sc, &damage)?;
            }
            if hovered == Some(index) {
                self.render_tooltip(
                    frame,
                    &control.tooltip,
                    slot_outer,
                    output_size,
                    scale,
                    false,
                )?;
            }
        }
        Ok(())
    }

    fn render_icon(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        icon: IconId,
        rect: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        output_size: Size<i32, Physical>,
    ) -> std::result::Result<(), GlesError> {
        self.render_icon_alpha(frame, icon, rect, scale, output_size, 1.0)
    }

    fn render_icon_alpha(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        icon: IconId,
        rect: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        output_size: Size<i32, Physical>,
        alpha: f32,
    ) -> std::result::Result<(), GlesError> {
        let Some(atlas) = self.atlas.as_ref() else {
            return Ok(());
        };
        let Some(entry) = atlas.get(icon) else {
            return Ok(());
        };
        let physical = rect.to_physical_precise_round(scale);
        render_atlas_icon_with_alpha(
            frame,
            &atlas.texture,
            *entry,
            physical.loc.x,
            physical.loc.y,
            physical.size.w,
            physical.size.h,
            output_size,
            alpha,
        )
    }

    /// Render one atlas icon through the shared glass-control shader.  The
    /// shader draws the rounded button well and derives the etched highlight
    /// and shadow directly from the icon alpha mask.
    fn draw_glass_icon(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        icon: IconId,
        control: Rectangle<i32, Logical>,
        icon_rect: Rectangle<i32, Logical>,
        scale: f64,
        output_size: Size<i32, Physical>,
        hovered: bool,
        control_state: &ShellControl,
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        let (Some(program), Some(background), Some(atlas)) = (
            self.shaders.glass_control.as_ref(),
            self.glass_background.as_ref(),
            self.atlas.as_ref(),
        ) else {
            return Ok(());
        };
        let Some(entry) = atlas.get(icon).copied() else {
            return Ok(());
        };
        let sc = Scale::from((scale, scale));
        let control: Rectangle<i32, Physical> = control.to_physical_precise_round(sc);
        let icon_rect: Rectangle<i32, Physical> = icon_rect.to_physical_precise_round(sc);
        if control.size.w <= 0 || control.size.h <= 0 {
            return Ok(());
        }
        let atlas_size = atlas.texture.size();
        // The shell has no scene texture to sample, so capture this control's
        // current framebuffer region into a texture before running the shader.
        let required_w = control.size.w;
        let required_h = control.size.h;
        let resized = true;
        let source = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (atlas_size.w as f64, atlas_size.h as f64),
        );
        let icon_uv_origin = [
            entry.x as f32 / atlas_size.w as f32,
            entry.y as f32 / atlas_size.h as f32,
        ];
        let icon_uv_size = [
            entry.w as f32 / atlas_size.w as f32,
            entry.h as f32 / atlas_size.h as f32,
        ];
        let icon_local_rect = [
            (icon_rect.loc.x - control.loc.x) as f32 / control.size.w as f32,
            (icon_rect.loc.y - control.loc.y) as f32 / control.size.h as f32,
            icon_rect.size.w as f32 / control.size.w as f32,
            icon_rect.size.h as f32 / control.size.h as f32,
        ];
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, background.tex_id());
            if resized {
                gl.TexImage2D(
                    ffi::TEXTURE_2D,
                    0,
                    ffi::RGBA as i32,
                    required_w,
                    required_h,
                    0,
                    ffi::RGBA,
                    ffi::UNSIGNED_BYTE,
                    std::ptr::null(),
                );
            }
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.CopyTexSubImage2D(
                ffi::TEXTURE_2D,
                0,
                0,
                0,
                control.loc.x,
                control.loc.y,
                control.size.w,
                control.size.h,
            );
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
        let result = frame.render_texture_from_to(
            &atlas.texture,
            source,
            control,
            &[Rectangle::from_size(output_size)],
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &[
                Uniform::new("u_background", 1i32),
                Uniform::new(
                    "u_background_uv_size",
                    [
                        control.size.w as f32 / required_w as f32,
                        control.size.h as f32 / required_h as f32,
                    ],
                ),
                Uniform::new("u_size", [control.size.w as f32, control.size.h as f32]),
                Uniform::new("u_icon_uv_origin", icon_uv_origin),
                Uniform::new("u_icon_uv_size", icon_uv_size),
                Uniform::new("u_icon_rect", icon_local_rect),
                Uniform::new(
                    "u_icon_texel_size",
                    [1.0 / atlas_size.w as f32, 1.0 / atlas_size.h as f32],
                ),
                // Shell controls need a stronger separation from the dark
                // bottom surface; the compositor's glass tint is deliberately
                // subtle and would make these standalone wells disappear.
                Uniform::new("u_glass_tint", [0.105f32, 0.17f32, 0.30f32, 0.92f32]),
                Uniform::new(
                    "u_accent_color",
                    [
                        self.theme.light.glow_color[0],
                        self.theme.light.glow_color[1],
                        self.theme.light.glow_color[2],
                    ],
                ),
                Uniform::new("u_corner_radius", self.theme.top_bar.radius * scale as f32),
                Uniform::new("u_border_width", 2.0f32),
                Uniform::new("u_hover", if hovered { 1.0f32 } else { 0.0 }),
                Uniform::new("u_pressed", 0.0f32),
                Uniform::new(
                    "u_enabled",
                    if control_state.enabled { 1.0f32 } else { 0.0 },
                ),
                Uniform::new(
                    "u_active",
                    if control_state.active || control_state.selected {
                        1.0f32
                    } else {
                        0.0
                    },
                ),
                Uniform::new("u_warning", 0.0f32),
                Uniform::new("u_light_dir", [-0.45f32, -0.65, 0.80]),
                Uniform::new("u_opacity", 0.96f32),
                Uniform::new("u_output_factor", 1.0f32),
                Uniform::new("u_icon_strength", 0.88f32),
                Uniform::new("u_etch_depth", 5.0f32),
            ],
        );
        let _ = frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
        });
        result
    }

    fn draw_pulse(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        rect: Rectangle<i32, Logical>,
        pulse: PulseFrame,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let Some(program) = self.shaders.pulse.as_ref() else {
            return Ok(());
        };
        let dst = rect.to_physical_precise_round(scale);
        let src = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (dst.size.w as f64, dst.size.h as f64),
        );
        let click_x =
            ((pulse.click.0 - rect.loc.x as f64) * scale.x).clamp(0.0, dst.size.w as f64) as f32;
        let click_y =
            ((pulse.click.1 - rect.loc.y as f64) * scale.y).clamp(0.0, dst.size.h as f64) as f32;
        frame.render_pixel_shader_to(
            program,
            src,
            dst,
            Size::<i32, Buffer>::from((dst.size.w, dst.size.h)),
            Some(damage),
            1.0,
            &[
                Uniform::new("u_click_pos", [click_x, click_y]),
                Uniform::new("u_time", pulse.elapsed.as_secs_f32()),
                Uniform::new("u_size", [dst.size.w as f32, dst.size.h as f32]),
                Uniform::new("u_color", [0.0f32, 0.5, 1.0, 1.0]),
            ],
        )
    }

    fn render_tooltip(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        text: &str,
        anchor: Rectangle<i32, Logical>,
        output_size: Size<i32, Physical>,
        scale: f64,
        panel: bool,
    ) -> Result<(), GlesError> {
        let max_chars = 38usize;
        let label = text.chars().take(max_chars).collect::<String>();
        let max_width = if panel { 300 } else { 232 };
        let width = (label.chars().count() as i32 * 8 + 20).clamp(80, max_width);
        let logical_surface_w = (output_size.w as f64 / scale).round() as i32;
        let logical_surface_h = (output_size.h as f64 / scale).round() as i32;
        let rect = if panel {
            Rectangle::from_loc_and_size(
                (
                    (anchor.loc.x + anchor.size.w / 2 - width / 2)
                        .clamp(4, logical_surface_w - width - 4),
                    70,
                ),
                (width, 34),
            )
        } else {
            Rectangle::from_loc_and_size(
                (84, (anchor.loc.y + 7).clamp(4, logical_surface_h - 38)),
                (width, 34),
            )
        };
        let sc = Scale::from((scale, scale));
        let damage = [Rectangle::from_size(output_size)];
        if let Some(beveled) = self.shaders.beveled_panel.as_ref() {
            draw_beveled_panel(frame, beveled, rect, sc, &damage, &self.theme.module)?;
        }
        self.render_text(
            frame,
            &label,
            ((rect.loc.x + 10) as f64 * scale).round() as i32,
            ((rect.loc.y + 23) as f64 * scale).round() as i32,
            output_size,
            scale,
            [0.95, 0.97, 1.0, 1.0],
        )
    }

    fn draw_topbar_icons(
        &self,
        frame: &mut impl Frame<
            TextureId = GlesTexture,
            Error = smithay::backend::renderer::gles::GlesError,
        >,
        output_size: Size<i32, Physical>,
    ) {
        let Some(atlas) = self.atlas.as_ref() else {
            return;
        };

        let x = self.metrics.sidebar_w + self.metrics.icon_padding;
        let y = (self.metrics.topbar_h - self.metrics.icon_base_px as i32) / 2;

        if let Some(entry) = atlas.get(IconId::Launcher) {
            let _ = render_atlas_icon(
                frame,
                &atlas.texture,
                *entry,
                x,
                y,
                self.metrics.icon_base_px as i32,
                self.metrics.icon_base_px as i32,
                output_size,
            );
        }
    }

    fn draw_sidebar_slots(
        &self,
        frame: &mut impl Frame<
            TextureId = GlesTexture,
            Error = smithay::backend::renderer::gles::GlesError,
        >,
        output_size: Size<i32, Physical>,
    ) {
        self.draw_sidebar_items(frame, output_size, 9);
    }

    fn draw_sidebar_items(
        &self,
        frame: &mut impl Frame<
            TextureId = GlesTexture,
            Error = smithay::backend::renderer::gles::GlesError,
        >,
        output_size: Size<i32, Physical>,
        workspace_count: usize,
    ) {
        let Some(atlas) = self.atlas.as_ref() else {
            return;
        };

        let base_x = (self.metrics.sidebar_w - self.metrics.icon_base_px as i32) / 2;
        let mut y = self.metrics.topbar_h + self.metrics.icon_padding;

        let mut icons = vec![IconId::Settings, IconId::Launcher];
        icons.extend((1..=workspace_count.clamp(1, 9)).map(|n| IconId::Slot(n as u8)));
        icons.extend([
            IconId::Plus,
            IconId::Browser,
            IconId::Terminal,
            IconId::Files,
            IconId::Email,
        ]);
        let max_items = ((output_size.h - self.metrics.icon_padding)
            / (self.metrics.icon_base_px as i32 + self.metrics.slot_spacing))
            .max(0) as usize;
        for icon in icons.into_iter().take(max_items) {
            if let Some(entry) = atlas.get(icon) {
                let _ = render_atlas_icon(
                    frame,
                    &atlas.texture,
                    *entry,
                    base_x,
                    y,
                    self.metrics.icon_base_px as i32,
                    self.metrics.icon_base_px as i32,
                    output_size,
                );
            }
            y += self.metrics.icon_base_px as i32 + self.metrics.slot_spacing;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_slots_start_in_dock_local_coordinates() {
        let slots = dock_slot_rects(76, 1080, 12);
        assert_eq!(slots.len(), 12);
        assert_eq!(slots[0].loc, (8, 10).into());
        assert_eq!(slots[0].size, (60, 48).into());
        assert!(slots.last().unwrap().loc.y + slots.last().unwrap().size.h <= 1080);
    }

    #[test]
    fn flow_field_icon_stays_square_in_a_wide_control() {
        let control = Rectangle::from_loc_and_size((10, 8), (138, 44));
        let icon = centered_square(control, control.size.h.min(28));
        assert_eq!(icon.size, (28, 28).into());
        assert_eq!(icon.loc, (65, 16).into());
    }
}
