use focaldesk_sounds::UiSoundBuffers;

pub struct RenderResources {
    // texture atlas
    // svg cache
    // text resources
    pub ui_sounds: UiSoundBuffers,
}

impl RenderResources {
    pub fn new() -> Self {
        Self {
            // initialize caches
            ui_sounds: UiSoundBuffers::generate(),
        }
    }
}
