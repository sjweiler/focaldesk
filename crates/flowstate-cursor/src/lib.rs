pub mod assets; // built-in RGBA cursor bitmaps / loader
pub mod cursor; // CursorIcon, CursorState
pub mod hardware; // generic hardware cursor state/buffers
pub mod hotspot; // CursorHotspot
pub mod image; // CursorImage
pub mod manager;
pub mod software; // software cursor rendering data
pub mod theme; // icon -> hotspot, maybe size metadata // high-level API used by compositor

pub use cursor::{CursorIcon, CursorState};
pub use hardware::CursorPlaneState;
pub use hotspot::CursorHotspot;
pub use manager::CursorManager;
pub use software::SoftwareCursorDest;
