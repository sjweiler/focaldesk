pub mod app;
pub mod input;
pub mod output;
pub mod render;
pub mod scene;
pub mod ui;
pub mod ui_state;
pub mod layout;
pub mod wallpaper;
pub mod consts;
pub mod wayland;
pub mod desktop;
pub mod shell;
pub mod window_store;
pub mod output_store;
pub mod workspace_store;
pub use flowstate_ui::{chrome_layout, chrome_shaders};
pub mod toplevel_interaction;
pub mod backend_render;
pub mod portal;
pub mod ui_builder;
pub mod focus;
pub mod fonts;

// Re-export the “top-level” state types so `crate::core::X` works everywhere.
pub use app::App;
pub use output::{OutputState};
pub use render::{FrameCtx, RenderState};
pub use scene::SceneState;
