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
