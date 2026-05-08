pub mod cursor;     // CursorIcon, CursorState
pub mod theme;      // icon -> hotspot, maybe size metadata
pub mod hotspot;     // CursorHotspot
pub mod image;       // CursorImage
pub mod assets;      // built-in RGBA cursor bitmaps / loader
pub mod hardware;    // generic hardware cursor state/buffers
pub mod software;    // software cursor rendering data
pub mod manager;     // high-level API used by compositor

pub use cursor::{CursorIcon, CursorState};
pub use hotspot::CursorHotspot;
pub use hardware::CursorPlaneState;
pub use manager::CursorManager;
pub use software::SoftwareCursorDest;
