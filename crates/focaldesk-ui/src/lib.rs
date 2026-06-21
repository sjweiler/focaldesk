#![allow(dead_code, deprecated)]

pub mod ai_permission;
pub mod atlas;
pub mod chrome;
pub mod element;
pub mod layout;
pub mod svg;
pub mod text;
pub mod types;
pub mod widgets;
#[derive(Default)]
pub struct UiState;
pub mod chrome_draw;
pub mod chrome_layout;
pub mod chrome_shaders;
pub mod chrome_theme;
pub mod clock;
pub mod desktop_frame;
pub mod desktop_output;
pub mod dialog;
pub mod dialog_layer;
pub mod dialog_layout;
pub mod dialog_render;
pub mod egui_layer;
pub mod egui_panels;
pub mod overlay;
pub mod portalpermission;
pub mod sidebar;
pub mod topbar;
pub mod ui_builder;
pub mod uicomponent;
pub mod uitree;
pub mod visual;
pub mod workarea;

pub use visual::{UiVisualState, UiVisualStyle, visual_style};

impl UiState {
    pub fn new() -> Self {
        Self
    }
}
