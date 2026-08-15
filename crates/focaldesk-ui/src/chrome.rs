use anyhow::{Result, anyhow};

use smithay::backend::renderer::Frame;
use smithay::backend::renderer::gles::{GlesFrame, GlesTexture, Uniform, ffi};
use smithay::backend::renderer::{ImportMem, Renderer, RendererSuper, Texture};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};
//use crate::icons::{IconCache, IconId, IconKey, IconState};
use crate::atlas::{IconAtlas, IconId, build_icon_atlas, render_atlas_icon};
use crate::chrome_draw::{draw_beveled_panel, draw_recessed_button, draw_top_bar, well_icon_rect};
use crate::chrome_layout::{ChromeLayoutConfig, build_chrome_layout_with_config};
use crate::chrome_shaders::ChromeShaders;
use crate::chrome_theme::{ChromeTheme, chrome_theme_from_flow_theme};
use crate::font_atlas::FontAtlas;
use crate::svg::rasterize_svg;
use focaldesk_themes::FlowTheme;
use image::GenericImageView;
use smithay::backend::allocator::Fourcc;

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
        tint: [f32; 4],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        if let Some(font_atlas) = &self.font_atlas {
            font_atlas.render(frame, text, x, baseline_y, output_size, tint)?;
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
    ) {
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
                status_item_count: crate::chrome_layout::DEFAULT_TOPBAR_STATUS_COUNT,
                sidebar_item_count: 0,
            },
        );
        if let Some(top_bar) = self.shaders.top_bar.as_ref() {
            let _ = draw_top_bar(
                frame,
                top_bar,
                layout.topbar.outer,
                sc,
                &damage,
                &self.theme.top_bar,
            );
        }
        if let Some(beveled) = beveled {
            for (rect, style) in [
                (layout.topbar.inner, &self.theme.frame_inner),
                (layout.topbar.title, &self.theme.panel_inner),
                (layout.topbar.trim, &self.theme.trim),
            ] {
                let _ = draw_beveled_panel(frame, beveled, rect, sc, &damage, style);
            }
        }
        if let Some(button) = button {
            for well in layout
                .topbar
                .status_wells
                .iter()
                .chain(std::iter::once(&layout.topbar.clock_well))
            {
                let _ = draw_recessed_button(frame, button, *well, sc, &damage, &self.theme.button);
            }
            let _ = draw_recessed_button(
                frame,
                button,
                layout.topbar.ai_button,
                sc,
                &damage,
                &self.theme.button,
            );
        }
        let launcher_control = layout.topbar.ai_button;
        let launcher_icon = well_icon_rect(launcher_control);
        if let (Some(atlas), Some(entry)) = (
            self.atlas.as_ref(),
            self.atlas.as_ref().and_then(|a| a.get(IconId::AiConsole)),
        ) {
            let physical = launcher_icon.to_physical_precise_round(sc);
            let _ = render_atlas_icon(
                frame,
                &atlas.texture,
                *entry,
                physical.loc.x,
                physical.loc.y,
                physical.size.w,
                physical.size.h,
                output_size,
            );
        }
    }

    /// Render only the sidebar portion for a standalone layer-shell dock.
    pub fn render_dock(
        &self,
        frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
        output_size: Size<i32, Physical>,
        workspace_count: usize,
        scale: f64,
    ) {
        let beveled = self.shaders.beveled_panel.as_ref();
        let button = self.shaders.recessed_button.as_ref();
        let damage = [Rectangle::from_size(output_size)];
        let sc = Scale::from((scale, scale));
        let logical_w = (output_size.w as f64 / scale) as i32;
        let logical_h = (output_size.h as f64 / scale) as i32;
        let requested_items = 2 + workspace_count.clamp(1, 9) + 5;
        let layout = build_chrome_layout_with_config(
            Size::from((logical_w, logical_h)),
            self.metrics.topbar_h,
            self.metrics.sidebar_w,
            ChromeLayoutConfig {
                status_item_count: 0,
                sidebar_item_count: requested_items,
            },
        );
        if let Some(beveled) = beveled {
            let _ = draw_beveled_panel(
                frame,
                beveled,
                layout.sidebar.outer,
                sc,
                &damage,
                &self.theme.sidebar,
            );
            let _ = draw_beveled_panel(
                frame,
                beveled,
                layout.sidebar.inner,
                sc,
                &damage,
                &self.theme.panel_inner,
            );
        }

        let icons = std::iter::once(IconId::Settings)
            .chain(std::iter::once(IconId::Launcher))
            .chain((1..=workspace_count.clamp(1, 9)).map(|n| IconId::Slot(n as u8)))
            .chain([
                IconId::Plus,
                IconId::Browser,
                IconId::Terminal,
                IconId::Files,
                IconId::Email,
            ]);
        for (slot, icon) in layout.sidebar.slots.iter().zip(icons) {
            if let Some(beveled) = beveled {
                let _ =
                    draw_beveled_panel(frame, beveled, slot.outer, sc, &damage, &self.theme.module);
                let _ = draw_beveled_panel(
                    frame,
                    beveled,
                    slot.inner,
                    sc,
                    &damage,
                    &self.theme.module_inner,
                );
            }
            if let Some(button) = button {
                let _ = draw_recessed_button(
                    frame,
                    button,
                    slot.icon_well,
                    sc,
                    &damage,
                    &self.theme.button,
                );
            }
            let icon_rect = well_icon_rect(slot.icon_well);
            if let (Some(atlas), Some(entry)) = (
                self.atlas.as_ref(),
                self.atlas.as_ref().and_then(|a| a.get(icon)),
            ) {
                let physical = icon_rect.to_physical_precise_round(sc);
                let _ = render_atlas_icon(
                    frame,
                    &atlas.texture,
                    *entry,
                    physical.loc.x,
                    physical.loc.y,
                    physical.size.w,
                    physical.size.h,
                    output_size,
                );
            }
        }
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
            &[Rectangle::from_size(control.size)],
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
                Uniform::new("u_hover", 0.0f32),
                Uniform::new("u_pressed", 0.0f32),
                Uniform::new("u_enabled", 1.0f32),
                Uniform::new("u_active", 0.0f32),
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
        let _ = output_size;
        result
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
