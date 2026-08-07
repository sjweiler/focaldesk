use anyhow::{anyhow, Result};
use image::{Rgba, RgbaImage};
use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{fontdb::Database, Options, Tree},
};

/// Rasterize an SVG into an RGBA pixmap at (width x height).
pub fn rasterize_svg(svg_bytes: &[u8], width: u32, height: u32) -> Result<RgbaImage> {
    let opt = Options::default();

    let mut fontdb = Database::new();
    fontdb.load_system_fonts();

    let tree = Tree::from_data(svg_bytes, &opt, &fontdb)
        .map_err(|e| anyhow!("failed to parse svg: {e}"))?;

    let mut pixmap =
        Pixmap::new(width, height).ok_or_else(|| anyhow!("failed to allocate pixmap"))?;

    let svg_size = tree.size();
    let sx = width as f32 / svg_size.width();
    let sy = height as f32 / svg_size.height();
    let transform = Transform::from_scale(sx, sy);

    let mut pmut = pixmap.as_mut();
    resvg::render(&tree, transform, &mut pmut);

    // Optional debug:
    // pixmap.save_png("/tmp/raster-test.png")?;

    let mut img = RgbaImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let p = pixmap
                .pixel(x, y)
                .ok_or_else(|| anyhow!("pixmap pixel out of bounds at ({x}, {y})"))?;

            let a = p.alpha();
            let (r, g, b) = if a == 0 {
                (0, 0, 0)
            } else {
                let a_u32 = a as u32;
                let r = ((p.red() as u32 * 255) / a_u32).min(255) as u8;
                let g = ((p.green() as u32 * 255) / a_u32).min(255) as u8;
                let b = ((p.blue() as u32 * 255) / a_u32).min(255) as u8;
                (r, g, b)
            };

            img.put_pixel(x, y, Rgba([r, g, b, a]));
        }
    }
    //img.save("/tmp/icon_rgba_test.png")?;
    Ok(img)
}
