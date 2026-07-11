use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

pub type Color = (u8, u8, u8);
pub type FontFace = Font;

static REGULAR: OnceLock<Font> = OnceLock::new();
static MEDIUM: OnceLock<Font> = OnceLock::new();

fn load_font(bytes: &'static [u8], label: &'static str) -> Font {
    Font::from_bytes(bytes, FontSettings::default())
        .unwrap_or_else(|err| panic!("failed to load {label}: {err:?}"))
}

pub fn regular() -> &'static FontFace {
    REGULAR.get_or_init(|| {
        load_font(
            include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
            "IBM Plex Sans Regular",
        )
    })
}

pub fn medium() -> &'static FontFace {
    MEDIUM.get_or_init(|| {
        load_font(
            include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf"),
            "IBM Plex Sans Medium",
        )
    })
}

pub fn measure_width(font: &FontFace, size: f32, text: &str) -> f32 {
    let mut width = 0.0;
    for ch in text.chars() {
        let metrics = font.rasterize(ch, size).0;
        width += metrics.advance_width;
    }
    width
}

pub fn line_height(font: &FontFace, size: f32) -> f32 {
    font.horizontal_line_metrics(size)
        .map(|metrics| metrics.ascent - metrics.descent + metrics.line_gap)
        .unwrap_or(size * 1.25)
}

fn blend_channel(dst: u8, src: u8, alpha: u8) -> u8 {
    let alpha = alpha as u16;
    let inv = 255u16.saturating_sub(alpha);
    (((dst as u16 * inv) + (src as u16 * alpha)) / 255) as u8
}

fn blend_pixel(buf: &mut [u8], pitch: u32, x: i32, y: i32, color: Color, alpha: u8) {
    if alpha == 0 || x < 0 || y < 0 {
        return;
    }
    let x = x as u32;
    let y = y as u32;
    let offset = (y * pitch + x * 4) as usize;
    if offset + 4 > buf.len() {
        return;
    }

    let b = color.2;
    let g = color.1;
    let r = color.0;
    buf[offset] = blend_channel(buf[offset], b, alpha);
    buf[offset + 1] = blend_channel(buf[offset + 1], g, alpha);
    buf[offset + 2] = blend_channel(buf[offset + 2], r, alpha);
    buf[offset + 3] = 0;
}

pub fn fill_rect(
    buf: &mut [u8],
    pitch: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Color,
    alpha: u8,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    for yy in y..y + h {
        for xx in x..x + w {
            blend_pixel(buf, pitch, xx, yy, color, alpha);
        }
    }
}

pub fn draw_border(
    buf: &mut [u8],
    pitch: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    thickness: i32,
    color: Color,
    alpha: u8,
) {
    if thickness <= 0 || w <= 0 || h <= 0 {
        return;
    }
    fill_rect(buf, pitch, x, y, w, thickness.min(h), color, alpha);
    fill_rect(
        buf,
        pitch,
        x,
        y + h - thickness,
        w,
        thickness.min(h),
        color,
        alpha,
    );
    fill_rect(buf, pitch, x, y, thickness.min(w), h, color, alpha);
    fill_rect(
        buf,
        pitch,
        x + w - thickness,
        y,
        thickness.min(w),
        h,
        color,
        alpha,
    );
}

pub fn draw_circle_ring(
    buf: &mut [u8],
    pitch: u32,
    cx: i32,
    cy: i32,
    outer_r: i32,
    inner_r: i32,
    color: Color,
    alpha: u8,
) {
    if outer_r <= 0 {
        return;
    }
    let outer_sq = outer_r * outer_r;
    let inner_sq = inner_r.max(0) * inner_r.max(0);
    for y in cy - outer_r..=cy + outer_r {
        for x in cx - outer_r..=cx + outer_r {
            let dx = x - cx;
            let dy = y - cy;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= outer_sq && dist_sq >= inner_sq {
                blend_pixel(buf, pitch, x, y, color, alpha);
            }
        }
    }
}

pub fn draw_line(
    buf: &mut [u8],
    pitch: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    thickness: i32,
    color: Color,
    alpha: u8,
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        fill_rect(
            buf,
            pitch,
            x0 - thickness / 2,
            y0 - thickness / 2,
            thickness,
            thickness,
            color,
            alpha,
        );
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

pub fn draw_text(
    buf: &mut [u8],
    pitch: u32,
    x: i32,
    baseline_y: i32,
    size: f32,
    color: Color,
    font: &FontFace,
    text: &str,
) {
    let mut caret = x as f32;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        let gx = caret as i32 + metrics.xmin;
        let gy = baseline_y - metrics.ymin - metrics.height as i32;

        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha != 0 {
                    blend_pixel(buf, pitch, gx + col as i32, gy + row as i32, color, alpha);
                }
            }
        }

        caret += metrics.advance_width;
    }
}

pub fn draw_text_centered(
    buf: &mut [u8],
    pitch: u32,
    center_x: i32,
    baseline_y: i32,
    size: f32,
    color: Color,
    font: &FontFace,
    text: &str,
) {
    let width = measure_width(font, size, text);
    let x = center_x - (width / 2.0).round() as i32;
    draw_text(buf, pitch, x, baseline_y, size, color, font, text);
}

pub fn wrap_to_width(font: &FontFace, size: f32, text: &str, max_width: i32) -> String {
    if max_width <= 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut width = 0.0;
    for ch in text.chars() {
        let advance = font.rasterize(ch, size).0.advance_width;
        if !out.is_empty() && width + advance > max_width as f32 {
            break;
        }
        out.push(ch);
        width += advance;
    }
    out
}

pub fn ellipsize(font: &FontFace, size: f32, text: &str, max_width: i32) -> String {
    if measure_width(font, size, text) <= max_width as f32 {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}");
        if measure_width(font, size, &candidate) > max_width as f32 {
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        "...".to_string()
    } else {
        format!("{out}…")
    }
}
