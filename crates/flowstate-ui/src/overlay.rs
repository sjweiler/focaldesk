use smithay::backend::renderer::gles::GlesFrame;
use smithay::utils::{Logical, Physical, Point, Rectangle};

use crate::desktop_frame::DesktopFrameCtx;
use crate::uicomponent::{UiHit, UiHitTarget};
use flowstate_types::WidgetId;

#[derive(Default)]
pub struct OverlayManager;

impl OverlayManager {
    pub fn render(
        &self,
        _frame: &mut GlesFrame<'_, '_>,
        _frame_ctx: &DesktopFrameCtx,
        _damage: &[Rectangle<i32, Physical>],
    ) {
    }

    pub fn hit_test(&self, _point: Point<i32, Logical>) -> Option<UiHit> {
        None
    }
}
