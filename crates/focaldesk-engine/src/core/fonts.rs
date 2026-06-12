use focaldesk_themes::theme::BuiltInThemeId;
use fontdue::Font;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontId {
    IbmPlexMonoRegular,
    IbmPlexSansRegular,
    IbmPlexSansMedium,
    IbmPlexSansSemiBold,

    RajdhaniRegular,
    RajdhaniMedium,
    RajdhaniSemiBold,

    OrbitronRegular,
    OrbitronMedium,
    OrbitronSemiBold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRole {
    Debug,
    Meta,
    Label,
    Title,
    Clock,
    Body,
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
    pub fn new(_theme_id: BuiltInThemeId) -> anyhow::Result<Self> {
        let mut system = Self::empty();

        // system.load_fallback_fonts()?;
        // system.load_theme_fonts(theme_id)?;

        system.load_all_fonts()?;

        Ok(system)
    }
    fn empty() -> Self {
        Self {
            fonts: HashMap::new(),
            glyphs: HashMap::new(),

            atlas_w: 1024,
            atlas_h: 1024,
            atlas_pixels: vec![0; 1024 * 1024],

            pen_x: 0,
            pen_y: 0,
            row_h: 0,

            atlas_dirty: false,
        }
    }

    fn load_font(
        &mut self,
        id: FontId,
        bytes: &'static [u8],
        label: &'static str,
    ) -> anyhow::Result<()> {
        let font = Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!("Failed to load {label}: {:?}", e))?;

        self.fonts.insert(id, font);
        Ok(())
    }

    fn load_fallback_fonts(&mut self) -> anyhow::Result<()> {
        self.load_font(
            FontId::IbmPlexMonoRegular,
            include_bytes!("../../../../assets/fonts/IBMPlexMono-Regular.ttf"),
            "IBM Plex Mono Regular",
        )?;

        Ok(())
    }

    fn load_all_fonts(&mut self) -> anyhow::Result<()> {
        self.load_font(
            FontId::IbmPlexMonoRegular,
            include_bytes!("../../../../assets/fonts/IBMPlexMono-Regular.ttf"),
            "IBM Plex Mono Regular",
        )?;

        self.load_font(
            FontId::IbmPlexSansRegular,
            include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf"),
            "IBM Plex Sans Regular",
        )?;

        self.load_font(
            FontId::IbmPlexSansMedium,
            include_bytes!("../../../../assets/fonts/IBMPlexSans-Medium.ttf"),
            "IBM Plex Sans Medium",
        )?;

        self.load_font(
            FontId::IbmPlexSansSemiBold,
            include_bytes!("../../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
            "IBM Plex Sans SemiBold",
        )?;

        self.load_font(
            FontId::RajdhaniRegular,
            include_bytes!("../../../../assets/fonts/Rajdhani-Regular.ttf"),
            "Rajdhani Regular",
        )?;

        self.load_font(
            FontId::RajdhaniMedium,
            include_bytes!("../../../../assets/fonts/Rajdhani-Medium.ttf"),
            "Rajdhani Medium",
        )?;

        self.load_font(
            FontId::RajdhaniSemiBold,
            include_bytes!("../../../../assets/fonts/Rajdhani-SemiBold.ttf"),
            "Rajdhani SemiBold",
        )?;

        self.load_font(
            FontId::OrbitronRegular,
            include_bytes!("../../../../assets/fonts/Orbitron-Regular.ttf"),
            "Orbitron Regular",
        )?;

        self.load_font(
            FontId::OrbitronMedium,
            include_bytes!("../../../../assets/fonts/Orbitron-Medium.ttf"),
            "Orbitron Medium",
        )?;

        self.load_font(
            FontId::OrbitronSemiBold,
            include_bytes!("../../../../assets/fonts/Orbitron-SemiBold.ttf"),
            "Orbitron SemiBold",
        )?;

        Ok(())
    }

    fn load_theme_fonts(&mut self, theme_id: BuiltInThemeId) -> anyhow::Result<()> {
        match theme_id {
            BuiltInThemeId::Classic => {
                self.load_font(
                    FontId::IbmPlexSansRegular,
                    include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf"),
                    "IBM Plex Sans Regular",
                )?;
                self.load_font(
                    FontId::IbmPlexSansMedium,
                    include_bytes!("../../../../assets/fonts/IBMPlexSans-Medium.ttf"),
                    "IBM Plex Sans Medium",
                )?;
                self.load_font(
                    FontId::IbmPlexSansSemiBold,
                    include_bytes!("../../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
                    "IBM Plex Sans SemiBold",
                )?;
            }

            BuiltInThemeId::Moonbase => {
                self.load_font(
                    FontId::RajdhaniRegular,
                    include_bytes!("../../../../assets/fonts/Rajdhani-Regular.ttf"),
                    "Rajdhani Regular",
                )?;
                self.load_font(
                    FontId::RajdhaniMedium,
                    include_bytes!("../../../../assets/fonts/Rajdhani-Medium.ttf"),
                    "Rajdhani Medium",
                )?;
                self.load_font(
                    FontId::RajdhaniSemiBold,
                    include_bytes!("../../../../assets/fonts/Rajdhani-SemiBold.ttf"),
                    "Rajdhani SemiBold",
                )?;
            }

            BuiltInThemeId::Eagle => {
                self.load_font(
                    FontId::OrbitronRegular,
                    include_bytes!("../../../../assets/fonts/Orbitron-Regular.ttf"),
                    "Orbitron Regular",
                )?;
                self.load_font(
                    FontId::OrbitronMedium,
                    include_bytes!("../../../../assets/fonts/Orbitron-Medium.ttf"),
                    "Orbitron Medium",
                )?;
                self.load_font(
                    FontId::OrbitronSemiBold,
                    include_bytes!("../../../../assets/fonts/Orbitron-SemiBold.ttf"),
                    "Orbitron SemiBold",
                )?;
            }
        }

        Ok(())
    }
    pub fn reload_for_theme(&mut self, theme_id: BuiltInThemeId) -> anyhow::Result<()> {
        self.fonts.clear();
        self.glyphs.clear();

        self.atlas_pixels.fill(0);
        self.pen_x = 0;
        self.pen_y = 0;
        self.row_h = 0;
        self.atlas_dirty = true;

        self.load_fallback_fonts()?;
        self.load_theme_fonts(theme_id)?;

        Ok(())
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

    /// Sum of horizontal advances in logical pixels. Must match spacing rules in
    /// `RenderState::draw_text_cached` (space width and missing-glyph skip).
    pub fn advance_width(&self, text: &str, style: TextStyle) -> i32 {
        let mut w = 0_i32;
        for ch in text.chars() {
            if ch == ' ' {
                w += style.size_px as i32 / 2;
                continue;
            }
            let Some(glyph) = self.glyph((style.font, style.size_px, ch)) else {
                continue;
            };
            w += glyph.advance.round() as i32;
        }
        w
    }

    fn prepare_glyph(&mut self, ch: char, style: TextStyle) -> anyhow::Result<()> {
        let key = (style.font, style.size_px, ch);

        if self.glyphs.contains_key(&key) {
            return Ok(());
        }

        let font = self
            .fonts
            .get(&style.font)
            .ok_or_else(|| anyhow::anyhow!("Missing font {:?}", style.font))?;

        let (metrics, bitmap) = font.rasterize(ch, style.size_px as f32);

        let w = metrics.width as u32;
        let h = metrics.height as u32;

        if w == 0 || h == 0 {
            self.glyphs.insert(
                key,
                GlyphEntry {
                    atlas_x: 0,
                    atlas_y: 0,
                    w: 0,
                    h: 0,
                    advance: metrics.advance_width,
                    xmin: metrics.xmin,
                    ymin: metrics.ymin,
                },
            );

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

        self.glyphs.insert(
            key,
            GlyphEntry {
                atlas_x,
                atlas_y,
                w,
                h,
                advance: metrics.advance_width,
                xmin: metrics.xmin,
                ymin: metrics.ymin,
            },
        );

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

pub fn style_for(role: FontRole, size_px: u32, theme: BuiltInThemeId) -> TextStyle {
    let font = match theme {
        BuiltInThemeId::Classic => match role {
            FontRole::Debug => FontId::IbmPlexMonoRegular,
            FontRole::Body => FontId::IbmPlexSansRegular,
            FontRole::Label | FontRole::Meta => FontId::IbmPlexSansMedium,
            FontRole::Title | FontRole::Clock => FontId::IbmPlexSansSemiBold,
        },

        BuiltInThemeId::Moonbase => match role {
            FontRole::Debug => FontId::IbmPlexMonoRegular,
            FontRole::Body => FontId::RajdhaniRegular,
            FontRole::Label | FontRole::Meta => FontId::RajdhaniMedium,
            FontRole::Title | FontRole::Clock => FontId::RajdhaniSemiBold,
        },

        BuiltInThemeId::Eagle => match role {
            FontRole::Debug => FontId::IbmPlexMonoRegular,
            FontRole::Body => FontId::IbmPlexSansRegular,
            FontRole::Label => FontId::IbmPlexSansMedium,
            FontRole::Meta | FontRole::Title | FontRole::Clock => FontId::OrbitronSemiBold,
        },
    };

    TextStyle { font, size_px }
}
