// crates/flowstate-engine/src/core/ui.rs
//use flowstate_ui::widgets::ClockCache;
use std::collections::HashMap;
use std::marker::PhantomData;
//use crate::core::chrome_sdf::{SdfBeveledPanel, SdfLightChannel};
use smithay::backend::renderer::gles::GlesRenderer;

use flowstate_ui::{chrome::Chrome, text::TextSystem, widgets::ClockCache};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeCacheKey {
    pub width: i32,
    pub height: i32,
    pub scale_milli: i32,
}

#[derive(Debug, Default)]
pub struct ChromeCacheState {
    pub top_key: Option<ChromeCacheKey>,
    pub side_key: Option<ChromeCacheKey>,
}

impl ChromeCacheState {
    pub fn new() -> Self {
        Self {
            top_key: None,
            side_key: None,
        }
    }
}

pub struct UiState<Tex: 'static> {
    pub chrome: Chrome,
    pub launcher_open: bool,
    pub text: flowstate_ui::text::TextSystem,
    pub clock: flowstate_ui::chrome::ClockCache,
    // pub sdf_light: Option<SdfLightChannel>,
    // pub sdf_panel: Option<SdfBeveledPanel>,
    pub chrome_cache: ChromeCacheState,
    // pub icons: IconCache<...>
    _phantom: PhantomData<Tex>,
}

impl<Tex: 'static> UiState<Tex> {
    pub fn chrome_mut(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    pub fn bootstrap() -> Self {
        Self::new(
            Chrome::new(flowstate_ui::chrome::ChromeMetrics::default()), // or Chrome::default()
            TextSystem::new(), // or TextSystem::default(),
            ClockCache::new(), // or ClockCache::default(),
            ChromeCacheState::new(),
        )
    }
    pub fn new(
        chrome: Chrome,
        text: TextSystem,
        clock: ClockCache,
        chrome_cache: ChromeCacheState,
    ) -> Self {
        Self {
            chrome,
            text,
            clock,
            //sdf_light:None,
            //sdf_panel: None,
            launcher_open: false,
            chrome_cache: ChromeCacheState::new(),
            _phantom: PhantomData,
        }
    }
    /*
    pub fn init_sdf_if_needed(&mut self, _renderer: &mut GlesRenderer) -> Result<(), String> {
        if self.sdf_light.is_none() {
            self.sdf_light = Some(SdfLightChannel::default());
        }

        if self.sdf_panel.is_none() {
            self.sdf_panel = Some(SdfBeveledPanel::new());
        }

        Ok(())
    }
    pub fn invalidate_chrome_cache(&mut self) {
        self.chrome_cache.top_key = None;
        self.chrome_cache.side_key = None;
        self.chrome.topbar_tex = None;
        self.chrome.sidebar_tex = None;
    }*/
}
