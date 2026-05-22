pub mod svg;
pub mod atlas;
pub mod chrome;
pub mod text;
pub mod layout;
pub mod widgets;
pub mod element;
pub mod types;
pub struct UiState;
pub mod uitree;
pub mod visual;
pub mod dialog;
pub mod dialog_layout;
pub mod dialog_layer;
pub mod chrome_layout;
pub mod chrome_shaders;
pub mod chrome_theme;
pub mod chrome_draw;
pub mod desktop_frame;
pub mod desktop_output;
pub mod dialog_render;
pub mod egui_layer;
pub mod topbar;
pub mod sidebar;
pub mod workarea;
pub mod uicomponent;
pub mod clock;
pub mod overlay;
pub mod ui_builder;

pub use visual::{
    UiVisualState,
    UiVisualStyle,
    visual_style,
};

impl UiState {
    pub fn new() -> Self {
        Self
    }
}
