//! One-shot UI atlas for the greeter.
//!
//! The DRM mode (and therefore every font size `render.rs` computes, see
//! `render::font_sizes`) is fixed for the life of the process, and the only
//! characters the greeter can ever be asked to draw are the printable
//! US-QWERTY ASCII set `keymap::keycode_to_char` produces. That makes the
//! full set of glyphs/icons this process will ever need to rasterize known
//! at startup, so we bake all of it into one GLES texture once instead of
//! re-rasterizing text and vector art on the CPU every frame.
//!
//! Everything in the atlas is stored as a white RGB + alpha-coverage
//! bitmap. Per-frame drawing recolors it with `TINTED_FRAG`, a tiny custom
//! texture shader (same mechanism the background gradient already uses via
//! `compile_custom_pixel_shader`, just the texture-sampling sibling
//! `compile_custom_texture_shader`) so one baked glyph or icon can be drawn
//! in any color without being baked once per color.

use std::collections::HashMap;

use anyhow::{Context, Result};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesRenderer, GlesTexProgram, GlesTexture, UniformName, UniformType,
};
use smithay::backend::renderer::ImportMem;
use smithay::utils::{Buffer, Rectangle, Size};

use crate::font::{self, FontFace};
use crate::render::{FontSizes, CURSOR_POLY};

/// Printable US-QWERTY ASCII range `keymap::keycode_to_char` can ever
/// produce, plus space.
const FIRST_CHAR: u32 = 0x20;
const LAST_CHAR: u32 = 0x7E;

/// Latin-1 Supplement, printable range. User-typed input is guaranteed
/// ASCII (bounded by `keymap::keycode_to_char`'s fixed US-QWERTY table),
/// but PAM prompt/error/notice text (`login::LoginState`'s `message`,
/// `error`, `notices` fields, sourced from the daemon's PAM conversation)
/// isn't — a localized PAM module can emit accented characters. This isn't
/// a complete Unicode guarantee (CJK/Cyrillic/etc. still silently drop,
/// see `GlyphAtlas::glyph`'s callers), but it covers Western European
/// locales cheaply.
const FIRST_LATIN1: u32 = 0xA0;
const LAST_LATIN1: u32 = 0xFF;

/// Characters `render.rs` can draw that fall outside the ranges above:
/// `font::ellipsize`'s truncation mark and the password-masking bullet used
/// by `field_text` for `AuthMessageStyle::Secret` input.
const EXTRA_CHARS: &[char] = &['\u{2026}', '\u{2022}'];

fn baked_chars() -> impl Iterator<Item = char> {
    (FIRST_CHAR..=LAST_CHAR)
        .chain(FIRST_LATIN1..=LAST_LATIN1)
        .filter_map(char::from_u32)
        .chain(EXTRA_CHARS.iter().copied())
}

const TINTED_FRAG: &str = r#"
#version 100

//_DEFINES_

precision mediump float;
uniform sampler2D tex;
uniform float alpha;
uniform vec3 u_color;
varying vec2 v_coords;

void main() {
    float coverage = texture2D(tex, v_coords).a;
    float a = coverage * alpha;
    gl_FragColor = vec4(u_color * a, a);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontId {
    Regular,
    Medium,
}

impl FontId {
    fn face(self) -> &'static FontFace {
        match self {
            FontId::Regular => font::regular(),
            FontId::Medium => font::medium(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconId {
    AvatarDisc,
    AvatarRing,
    PowerIcon,
    CursorOutline,
    CursorBody,
    SpinnerDot,
}

/// Fixed pixel sizes for the baked vector icons, chosen to match the sizes
/// `render.rs`'s CPU path already draws at (avatar radius 27, avatar ring
/// 30/24, power icon within a 56x56 button, spinner dot radius 3). The
/// cursor arrow (`render::CURSOR_POLY`) spans roughly 11x20; outline and
/// body share one 13x22 canvas (1px margin for the outline's ±1 offset)
/// so both icons can be drawn at the same destination rect.
const AVATAR_DISC_R: i32 = 27;
const AVATAR_RING_OUTER: i32 = 30;
const AVATAR_RING_INNER: i32 = 24;
const POWER_ICON_SIZE: i32 = 56;
const CURSOR_CANVAS_W: u32 = 13;
const CURSOR_CANVAS_H: u32 = 22;
const SPINNER_DOT_R: i32 = 3;

#[derive(Clone, Copy, Debug, Default)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl AtlasRect {
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::new(
            (self.x as f64, self.y as f64).into(),
            (self.w as f64, self.h as f64).into(),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GlyphInfo {
    /// `None` for zero-area glyphs (space) — nothing to draw, only advance.
    pub rect: Option<AtlasRect>,
    pub xmin: i32,
    pub ymin: i32,
    pub advance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SizeKey(u32);

impl SizeKey {
    fn new(size: f32) -> Self {
        Self((size * 10.0).round() as u32)
    }
}

/// A fresh RGBA8 (straight, non-premultiplied alpha) scratch canvas used to
/// bake the vector icons. Distinct from `font.rs`'s blend helpers, which
/// composite BGRX straight into the live XRGB8888 scanout buffer — this
/// composites white-on-transparent for later GPU tinting instead.
struct Canvas {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            data: vec![0u8; (w * h * 4) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, coverage: u8) {
        if coverage == 0 || x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let idx = ((y as u32 * self.w + x as u32) * 4) as usize;
        let a = self.data[idx + 3].max(coverage);
        self.data[idx] = 255;
        self.data[idx + 1] = 255;
        self.data[idx + 2] = 255;
        self.data[idx + 3] = a;
    }

    fn fill_circle(&mut self, cx: i32, cy: i32, r: i32) {
        let r2 = r * r;
        for y in cy - r..=cy + r {
            for x in cx - r..=cx + r {
                let dx = x - cx;
                let dy = y - cy;
                if dx * dx + dy * dy <= r2 {
                    self.blend(x, y, 255);
                }
            }
        }
    }

    fn ring(&mut self, cx: i32, cy: i32, outer: i32, inner: i32) {
        let outer_sq = outer * outer;
        let inner_sq = inner.max(0) * inner.max(0);
        for y in cy - outer..=cy + outer {
            for x in cx - outer..=cx + outer {
                let dx = x - cx;
                let dy = y - cy;
                let dist_sq = dx * dx + dy * dy;
                if dist_sq <= outer_sq && dist_sq >= inner_sq {
                    self.blend(x, y, 255);
                }
            }
        }
    }

    fn thick_line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, thickness: i32) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            for yy in y0 - thickness / 2..y0 - thickness / 2 + thickness {
                for xx in x0 - thickness / 2..x0 - thickness / 2 + thickness {
                    self.blend(xx, yy, 255);
                }
            }
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

    // Even-odd scanline fill, mirroring `render.rs::fill_polygon`.
    fn polygon(&mut self, points: &[(f32, f32)], ox: i32, oy: i32) {
        let (Some(min_y), Some(max_y)) = (
            points.iter().map(|p| p.1).reduce(f32::min),
            points.iter().map(|p| p.1).reduce(f32::max),
        ) else {
            return;
        };

        for y in min_y.floor() as i32..=max_y.ceil() as i32 {
            let yf = y as f32 + 0.5;
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..points.len() {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % points.len()];
                if (y1 <= yf && y2 > yf) || (y2 <= yf && y1 > yf) {
                    xs.push(x1 + (yf - y1) / (y2 - y1) * (x2 - x1));
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());

            for pair in xs.chunks_exact(2) {
                for x in pair[0].round() as i32..pair[1].round() as i32 {
                    self.blend(ox + x, oy + y, 255);
                }
            }
        }
    }
}

struct Sprite {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

type GlyphKey = (FontId, SizeKey, char);
/// A rasterized glyph awaiting packing: its atlas key, pixel data, and the
/// fontdue metrics (`xmin`, `ymin`, `advance_width`) needed to place it.
type GlyphSprite = (GlyphKey, Sprite, i32, i32, f32);

/// Simple shelf packer. The full sprite set is known upfront (fixed glyph
/// set at fixed sizes, fixed icon set), so a single pass with no dynamic
/// growth is enough.
struct ShelfPacker {
    width: u32,
    x: u32,
    y: u32,
    shelf_h: u32,
}

impl ShelfPacker {
    fn new(width: u32) -> Self {
        Self {
            width,
            x: 0,
            y: 0,
            shelf_h: 0,
        }
    }

    fn place(&mut self, w: u32, h: u32) -> AtlasRect {
        if w == 0 || h == 0 {
            return AtlasRect::default();
        }
        if self.x + w > self.width {
            self.y += self.shelf_h + 1;
            self.x = 0;
            self.shelf_h = 0;
        }
        let rect = AtlasRect {
            x: self.x,
            y: self.y,
            w,
            h,
        };
        self.x += w + 1;
        self.shelf_h = self.shelf_h.max(h);
        rect
    }

    fn total_height(&self) -> u32 {
        self.y + self.shelf_h
    }
}

pub struct GlyphAtlas {
    texture: GlesTexture,
    program: GlesTexProgram,
    glyphs: HashMap<GlyphKey, GlyphInfo>,
    icons: HashMap<IconId, AtlasRect>,
}

impl GlyphAtlas {
    pub fn build(renderer: &mut GlesRenderer, sizes: FontSizes) -> Result<Self> {
        let mut font_sizes: Vec<(FontId, f32)> = vec![
            (FontId::Medium, sizes.title),
            (FontId::Medium, sizes.avatar),
            (FontId::Regular, sizes.body),
            (FontId::Regular, sizes.small),
            (FontId::Medium, sizes.field),
            (FontId::Regular, sizes.field),
        ];
        font_sizes.dedup_by_key(|(font, size)| (*font, SizeKey::new(*size)));

        let mut glyph_sprites: Vec<GlyphSprite> = Vec::new();
        for (font_id, size) in &font_sizes {
            let face = font_id.face();
            for ch in baked_chars() {
                let (metrics, bitmap) = face.rasterize(ch, *size);
                let key = (*font_id, SizeKey::new(*size), ch);
                let sprite = Sprite {
                    w: metrics.width as u32,
                    h: metrics.height as u32,
                    rgba: coverage_to_rgba(&bitmap),
                };
                glyph_sprites.push((
                    key,
                    sprite,
                    metrics.xmin,
                    metrics.ymin,
                    metrics.advance_width,
                ));
            }
        }

        let icon_sprites = build_icon_sprites();

        // Pass 1: pack (pure geometry, no pixels touched yet).
        const ATLAS_WIDTH: u32 = 1024;
        let mut packer = ShelfPacker::new(ATLAS_WIDTH);
        let mut glyph_rects: Vec<Option<AtlasRect>> = Vec::with_capacity(glyph_sprites.len());
        for (_, sprite, ..) in &glyph_sprites {
            glyph_rects.push(if sprite.w == 0 || sprite.h == 0 {
                None
            } else {
                Some(packer.place(sprite.w, sprite.h))
            });
        }
        let mut icon_rects: Vec<AtlasRect> = Vec::with_capacity(icon_sprites.len());
        for (_, sprite) in &icon_sprites {
            icon_rects.push(packer.place(sprite.w, sprite.h));
        }
        let atlas_height = packer.total_height().max(1);

        // Pass 2: blit into the final canvas.
        let mut canvas = vec![0u8; (ATLAS_WIDTH * atlas_height * 4) as usize];
        for ((_, sprite, ..), rect) in glyph_sprites.iter().zip(glyph_rects.iter()) {
            if let Some(rect) = rect {
                blit(
                    &mut canvas,
                    ATLAS_WIDTH,
                    &sprite.rgba,
                    sprite.w,
                    sprite.h,
                    rect,
                );
            }
        }
        for ((_, sprite), rect) in icon_sprites.iter().zip(icon_rects.iter()) {
            blit(
                &mut canvas,
                ATLAS_WIDTH,
                &sprite.rgba,
                sprite.w,
                sprite.h,
                rect,
            );
        }

        let texture = renderer
            .import_memory(
                &canvas,
                Fourcc::Abgr8888,
                Size::from((ATLAS_WIDTH as i32, atlas_height as i32)),
                false,
            )
            .context("failed to upload greeter glyph atlas texture")?;

        let program = renderer
            .compile_custom_texture_shader(
                TINTED_FRAG,
                &[UniformName::new("u_color", UniformType::_3f)],
            )
            .context("failed to compile greeter glyph tint shader")?;

        let mut glyphs = HashMap::with_capacity(glyph_sprites.len());
        for ((key, _, xmin, ymin, advance), rect) in glyph_sprites.into_iter().zip(glyph_rects) {
            glyphs.insert(
                key,
                GlyphInfo {
                    rect,
                    xmin,
                    ymin,
                    advance,
                },
            );
        }

        let mut icons = HashMap::with_capacity(icon_sprites.len());
        for ((id, _), rect) in icon_sprites.into_iter().zip(icon_rects) {
            icons.insert(id, rect);
        }

        Ok(Self {
            texture,
            program,
            glyphs,
            icons,
        })
    }

    pub fn texture(&self) -> &GlesTexture {
        &self.texture
    }

    pub fn program(&self) -> &GlesTexProgram {
        &self.program
    }

    pub fn glyph(&self, font: FontId, size: f32, ch: char) -> Option<&GlyphInfo> {
        self.glyphs.get(&(font, SizeKey::new(size), ch))
    }

    pub fn icon_rect(&self, id: IconId) -> AtlasRect {
        self.icons.get(&id).copied().unwrap_or_default()
    }

    pub fn icon_src(&self, id: IconId) -> Rectangle<f64, Buffer> {
        self.icon_rect(id).src()
    }

    pub fn glyph_src(&self, info: &GlyphInfo) -> Option<Rectangle<f64, Buffer>> {
        info.rect.map(|r| r.src())
    }
}

fn coverage_to_rgba(coverage: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(coverage.len() * 4);
    for &a in coverage {
        out.extend_from_slice(&[255, 255, 255, a]);
    }
    out
}

fn blit(canvas: &mut [u8], canvas_w: u32, src: &[u8], w: u32, h: u32, dst: &AtlasRect) {
    for row in 0..h {
        let src_start = (row * w * 4) as usize;
        let src_end = src_start + (w * 4) as usize;
        let dst_start = (((dst.y + row) * canvas_w + dst.x) * 4) as usize;
        let dst_end = dst_start + (w * 4) as usize;
        canvas[dst_start..dst_end].copy_from_slice(&src[src_start..src_end]);
    }
}

fn build_icon_sprites() -> Vec<(IconId, Sprite)> {
    let mut out = Vec::new();

    let mut disc = Canvas::new(
        (AVATAR_DISC_R * 2 + 1) as u32,
        (AVATAR_DISC_R * 2 + 1) as u32,
    );
    disc.fill_circle(AVATAR_DISC_R, AVATAR_DISC_R, AVATAR_DISC_R);
    out.push((
        IconId::AvatarDisc,
        Sprite {
            w: disc.w,
            h: disc.h,
            rgba: disc.data,
        },
    ));

    let mut ring = Canvas::new(
        (AVATAR_RING_OUTER * 2 + 1) as u32,
        (AVATAR_RING_OUTER * 2 + 1) as u32,
    );
    ring.ring(
        AVATAR_RING_OUTER,
        AVATAR_RING_OUTER,
        AVATAR_RING_OUTER,
        AVATAR_RING_INNER,
    );
    out.push((
        IconId::AvatarRing,
        Sprite {
            w: ring.w,
            h: ring.h,
            rgba: ring.data,
        },
    ));

    // Mirrors `render.rs::draw_power_icon`, baked for a POWER_ICON_SIZE
    // square (the power button is always drawn at that fixed size).
    let mut power = Canvas::new(POWER_ICON_SIZE as u32, POWER_ICON_SIZE as u32);
    let cx = POWER_ICON_SIZE / 2;
    let cy = POWER_ICON_SIZE / 2 - 1;
    let outer = POWER_ICON_SIZE / 2 - 4;
    let inner = outer - 4;
    power.ring(cx, cy, outer, inner);
    power.thick_line(cx, cy - outer + 2, cx, cy - inner + 1, 3);
    out.push((
        IconId::PowerIcon,
        Sprite {
            w: power.w,
            h: power.h,
            rgba: power.data,
        },
    ));

    // Cursor outline (black silhouette, offset ±1px in every direction)
    // and body (white arrow) share one canvas size so both can be drawn at
    // the same destination rect, matching `render.rs::draw_cursor`.
    let mut cursor_outline = Canvas::new(CURSOR_CANVAS_W, CURSOR_CANVAS_H);
    for (dx, dy) in [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ] {
        cursor_outline.polygon(CURSOR_POLY, 1 + dx, 1 + dy);
    }
    out.push((
        IconId::CursorOutline,
        Sprite {
            w: cursor_outline.w,
            h: cursor_outline.h,
            rgba: cursor_outline.data,
        },
    ));

    let mut cursor_body = Canvas::new(CURSOR_CANVAS_W, CURSOR_CANVAS_H);
    cursor_body.polygon(CURSOR_POLY, 1, 1);
    out.push((
        IconId::CursorBody,
        Sprite {
            w: cursor_body.w,
            h: cursor_body.h,
            rgba: cursor_body.data,
        },
    ));

    let mut dot = Canvas::new(
        (SPINNER_DOT_R * 2 + 1) as u32,
        (SPINNER_DOT_R * 2 + 1) as u32,
    );
    dot.fill_circle(SPINNER_DOT_R, SPINNER_DOT_R, SPINNER_DOT_R);
    out.push((
        IconId::SpinnerDot,
        Sprite {
            w: dot.w,
            h: dot.h,
            rgba: dot.data,
        },
    ));

    out
}
