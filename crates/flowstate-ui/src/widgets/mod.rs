pub mod clock;
pub use crate::chrome::ClockCache;

use smithay::utils::{Point, Size};

/// A simple “widget context” you can extend later.
/// Keep it minimal; the compositor (engine) can fill it in each frame/tick.
pub struct WidgetCtx<'a> {
    pub now_utc: chrono::DateTime<chrono::Utc>,
    pub scale: f64,
    // later: locale, timezone, battery %, net status, etc.
    pub text: &'a mut crate::text::TextSystem,
}

/// Where to draw it.
#[derive(Clone, Copy, Debug)]
pub struct WidgetRect {
    pub loc: Point<i32, smithay::utils::Physical>,
    pub size: Size<i32, smithay::utils::Physical>,
}
