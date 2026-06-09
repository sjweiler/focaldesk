use smithay::backend::renderer::gles::GlesFrame;
use smithay::utils::{Logical, Physical, Point, Rectangle};

use crate::desktop_frame::DesktopFrameCtx;
use crate::uicomponent::LayoutCtx;
use crate::uicomponent::RenderCtx;
use crate::uicomponent::UiComponent;
use crate::uicomponent::UiHit;
use smithay::backend::renderer::gles::GlesError;

#[derive(Default)]
pub struct OverlayManager {
    active: Vec<OverlayKind>,
}

pub enum OverlayKind {
    Settings,
    Launcher,
    CommandPalette,
    AppSwitcher,
    WorkspaceSwitcher,
    Notifications,
    PowerMenu,
    ScreenshotTool,
    DisplayLayout,
    DebugHud,
    AiAssistant,
}

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

impl UiComponent for OverlayManager {
    fn layout(&mut self, _ctx: &LayoutCtx) {}
    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        self.hit_test(point)
    }
    fn render(&self, _ctx: &mut RenderCtx) -> Result<(), GlesError> {
        Ok(())
    }
}
