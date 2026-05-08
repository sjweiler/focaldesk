use std::collections::HashMap;
use fontdue::Font;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontId {
    DebugMono,
    PrimarySemiBold,
    PrimaryRegular,
    PrimaryMedium,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextStyle {
    pub font: FontId,
    pub size_px: u32,
}

#[derive(Clone, Debug)]
pub struct GlyphEntry {
    pub atlas_x: u32,
    pub atlas_y: u32,
    pub w: u32,
    pub h: u32,

    pub advance: f32,
    pub xmin: i32,
    pub ymin: i32,
}

#[derive(Clone, Debug)]
pub struct FontSystem {
    fonts: HashMap<FontId, Font>,
    glyphs: HashMap<(FontId, u32, char), GlyphEntry>,

    // CPU-side atlas for now
    atlas_w: u32,
    atlas_h: u32,
    atlas_pixels: Vec<u8>,

    pen_x: u32,
    pen_y: u32,
    row_h: u32,

    pub atlas_dirty: bool,
}


impl FontSystem {
    pub fn new() -> anyhow::Result<Self> {
        let debug_font_bytes =
            include_bytes!("../../../../assets/fonts/IBMPlexMono-Regular.ttf");

        let debug_font = Font::from_bytes(
            debug_font_bytes as &[u8],
            fontdue::FontSettings::default(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to load debug font: {:?}", e))?;

        let mut fonts = HashMap::new();
        fonts.insert(FontId::DebugMono, debug_font);

        Ok(Self {
            fonts,
            glyphs: HashMap::new(),

            atlas_w: 1024,
            atlas_h: 1024,
            atlas_pixels: vec![0; 1024 * 1024],

            pen_x: 0,
            pen_y: 0,
            row_h: 0,

            atlas_dirty: false,
        })
    }
    
    pub fn glyph(&self, key: (FontId, u32, char)) -> Option<&GlyphEntry> {
        self.glyphs.get(&key)
    }

    
    pub fn prepare_text(&mut self, text: &str, style: TextStyle) -> anyhow::Result<()> {
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }

            self.prepare_glyph(ch, style)?;
        }

        Ok(())
    }

    fn prepare_glyph(&mut self, ch: char, style: TextStyle) -> anyhow::Result<()> {
        let key = (style.font, style.size_px, ch);

        if self.glyphs.contains_key(&key) {
            return Ok(());
        }

        let font = self.fonts
            .get(&style.font)
            .ok_or_else(|| anyhow::anyhow!("Missing font {:?}", style.font))?;

        let (metrics, bitmap) = font.rasterize(ch, style.size_px as f32);

        let w = metrics.width as u32;
        let h = metrics.height as u32;

        if w == 0 || h == 0 {
            self.glyphs.insert(key, GlyphEntry {
                atlas_x: 0,
                atlas_y: 0,
                w: 0,
                h: 0,
                advance: metrics.advance_width,
                xmin: metrics.xmin,
                ymin: metrics.ymin,
            });

            return Ok(());
        }

        if self.pen_x + w >= self.atlas_w {
            self.pen_x = 0;
            self.pen_y += self.row_h + 1;
            self.row_h = 0;
        }

        if self.pen_y + h >= self.atlas_h {
            return Err(anyhow::anyhow!("Font atlas full"));
        }

        let atlas_x = self.pen_x;
        let atlas_y = self.pen_y;

        for y in 0..h {
            for x in 0..w {
                let src = bitmap[(y * w + x) as usize];
                let dst_index = ((atlas_y + y) * self.atlas_w + (atlas_x + x)) as usize;
                self.atlas_pixels[dst_index] = src;
            }
        }

        self.glyphs.insert(key, GlyphEntry {
            atlas_x,
            atlas_y,
            w,
            h,
            advance: metrics.advance_width,
            xmin: metrics.xmin,
            ymin: metrics.ymin,
        });

        self.pen_x += w + 1;
        self.row_h = self.row_h.max(h);
        self.atlas_dirty = true;

        Ok(())
    }
    
    pub fn atlas_size(&self) -> (u32, u32) {
        (self.atlas_w, self.atlas_h)
    }

    pub fn atlas_pixels(&self) -> &[u8] {
        &self.atlas_pixels
    }

    pub fn clear_dirty(&mut self) {
        self.atlas_dirty = false;
    }
}


