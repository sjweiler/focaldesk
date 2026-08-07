use anyhow::{Result, anyhow};
use fontdue::Font;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use std::collections::HashMap;

const ATLAS_W: u32 = 1024;
const ATLAS_H: u32 = 256;
const FONT_SIZE: u32 = 24;

#[derive(Clone, Copy)]
struct Glyph {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    advance: f32,
    xmin: i32,
    ymin: i32,
}

pub struct FontAtlas {
    pixels: Vec<u8>,
    glyphs: HashMap<char, Glyph>,
    texture: Option<smithay::backend::renderer::gles::GlesTexture>,
    shader: Option<GlesTexProgram>,
}

impl FontAtlas {
    pub fn new() -> Result<Self> {
        let font = Font::from_bytes(
            crate::fonts::IBMPLEX_REGULAR,
            fontdue::FontSettings::default(),
        )
        .map_err(|e| anyhow!("load IBM Plex Sans: {e:?}"))?;
        let mut coverage = vec![0u8; (ATLAS_W * ATLAS_H) as usize];
        let mut glyphs = HashMap::new();
        let mut pen_x = 0u32;
        let mut pen_y = 0u32;
        let mut row_h = 0u32;
        for ch in (32u8..=126).map(char::from) {
            let (metrics, bitmap) = font.rasterize(ch, FONT_SIZE as f32);
            let (w, h) = (metrics.width as u32, metrics.height as u32);
            if w == 0 || h == 0 {
                glyphs.insert(
                    ch,
                    Glyph {
                        x: 0,
                        y: 0,
                        w: 0,
                        h: 0,
                        advance: metrics.advance_width,
                        xmin: metrics.xmin,
                        ymin: metrics.ymin,
                    },
                );
                continue;
            }
            if pen_x + w >= ATLAS_W {
                pen_x = 0;
                pen_y += row_h + 1;
                row_h = 0;
            }
            if pen_y + h >= ATLAS_H {
                return Err(anyhow!("IBM Plex glyph atlas full"));
            }
            for y in 0..h {
                for x in 0..w {
                    coverage[((pen_y + y) * ATLAS_W + pen_x + x) as usize] =
                        bitmap[(y * w + x) as usize];
                }
            }
            glyphs.insert(
                ch,
                Glyph {
                    x: pen_x,
                    y: pen_y,
                    w,
                    h,
                    advance: metrics.advance_width,
                    xmin: metrics.xmin,
                    ymin: metrics.ymin,
                },
            );
            pen_x += w + 1;
            row_h = row_h.max(h);
        }
        let mut pixels = Vec::with_capacity(coverage.len() * 4);
        for value in coverage {
            pixels.extend_from_slice(&[value, value, value, value]);
        }
        Ok(Self {
            pixels,
            glyphs,
            texture: None,
            shader: None,
        })
    }

    pub fn ensure_uploaded(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        if self.texture.is_none() {
            self.texture = Some(renderer.import_memory(
                &self.pixels,
                Fourcc::Abgr8888,
                Size::from((ATLAS_W as i32, ATLAS_H as i32)),
                false,
            )?);
        }
        if self.shader.is_none() {
            self.shader = Some(renderer.compile_custom_texture_shader(
                FONT_TEXT_FRAG,
                &[UniformName::new("u_tint", UniformType::_4f)],
            )?);
        }
        Ok(())
    }

    pub fn render(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        text: &str,
        x: i32,
        baseline_y: i32,
        output_size: Size<i32, Physical>,
        tint: [f32; 4],
    ) -> Result<(), GlesError> {
        let (Some(texture), Some(shader)) = (&self.texture, &self.shader) else {
            return Ok(());
        };
        let mut cursor_x = x;
        for ch in text.chars() {
            let Some(glyph) = self.glyphs.get(&ch) else {
                continue;
            };
            if glyph.w == 0 || glyph.h == 0 {
                cursor_x += glyph.advance.round() as i32;
                continue;
            }
            let src = Rectangle::<f64, Buffer>::from_loc_and_size(
                (glyph.x as f64, glyph.y as f64),
                (glyph.w as f64, glyph.h as f64),
            );
            let dst = Rectangle::<i32, Physical>::from_loc_and_size(
                (
                    cursor_x + glyph.xmin,
                    baseline_y - glyph.ymin - glyph.h as i32,
                ),
                (glyph.w as i32, glyph.h as i32),
            );
            frame.render_texture_from_to(
                texture,
                src,
                dst,
                &[Rectangle::from_size(output_size)],
                &[],
                Transform::Normal,
                1.0,
                Some(shader),
                &[Uniform::new("u_tint", tint)],
            )?;
            cursor_x += glyph.advance.round() as i32;
        }
        Ok(())
    }
}

const FONT_TEXT_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif
varying vec2 v_coords;
uniform sampler2D tex;
uniform vec4 u_tint;
void main() {
    float cov = min(texture2D(tex, v_coords).r, min(texture2D(tex, v_coords).g, texture2D(tex, v_coords).b));
    gl_FragColor = vec4(u_tint.rgb * cov * u_tint.a, cov * u_tint.a);
}
"#;
