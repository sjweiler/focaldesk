use anyhow::{Result, anyhow};

use smithay::backend::renderer::Frame;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::{ImportMem, Renderer, RendererSuper};
use smithay::utils::{Physical, Rectangle, Size};
//use crate::icons::{IconCache, IconId, IconKey, IconState};
use crate::atlas::{IconAtlas, IconId, build_icon_atlas, render_atlas_icon};
use crate::svg::rasterize_svg;
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
}

impl Chrome {
    pub fn new(metrics: ChromeMetrics) -> Self {
        Self {
            metrics,
            //icon_cache: None,
            topbar_tex: None,
            sidebar_tex: None,
            atlas: None,
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
        let Some(atlas) = self.atlas.as_ref() else {
            return;
        };

        let base_x = (self.metrics.sidebar_w - self.metrics.icon_base_px as i32) / 2;
        let mut y = self.metrics.topbar_h + self.metrics.icon_padding;

        for n in 1..=9 {
            if let Some(entry) = atlas.get(IconId::Slot(n)) {
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
