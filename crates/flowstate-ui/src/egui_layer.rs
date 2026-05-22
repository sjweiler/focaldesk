//! egui overlay — rendered last, above dialogs and compositor chrome.

use smithay::backend::renderer::gles::GlesFrame;
use smithay::utils::Physical;
use smithay::utils::Rectangle;

use crate::desktop_frame::DesktopFrameCtx;

pub struct EguiLayer;

impl EguiLayer {
    pub fn render(
        &self,
        _frame: &mut GlesFrame<'_, '_>,
        _frame_ctx: &DesktopFrameCtx,
        _damage: &[Rectangle<i32, Physical>],
    ) {
        // Future: tessellate egui draw lists into the GLES frame.
    }
}
