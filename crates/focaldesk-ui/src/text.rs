use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};

pub struct TextSystem {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

pub fn rasterize_text_to_texture(
    _renderer: &mut GlesRenderer,
    _text: &str,
    _scale: f64,
) -> Option<GlesTexture> {
    // TODO: implement real rasterization later
    None
}

impl TextSystem {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    pub fn create_buffer(&mut self, text: &str, size: f32) -> Buffer {
        let metrics = Metrics::new(size, size * 1.2);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);

        buffer.set_text(
            &mut self.font_system,
            text,
            Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
        );

        buffer
    }
}
