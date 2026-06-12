use crate::chrome_layout::ChromeLayout;
use crate::element::UiElement;
use crate::element::UiRect;
use crate::uicomponent::LayoutCtx;
use crate::uicomponent::RenderCtx;
use crate::uicomponent::UiComponent;
use crate::uicomponent::UiHit;
use crate::uicomponent::UiHitTarget;
use focaldesk_types::WidgetId;
use smithay::backend::renderer::gles::GlesError;
use smithay::utils::Logical;
use smithay::utils::Point;

pub struct SideBar {
    pub buttons: Vec<UiElement>,
    pub workspace_buttons: Vec<UiElement>,
    pub bounds: UiRect,
    pub elements: Vec<UiElement>,
}

impl Default for SideBar {
    fn default() -> Self {
        Self {
            buttons: Vec::new(),
            workspace_buttons: Vec::new(),
            bounds: UiRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            elements: Vec::new(),
        }
    }
}

impl SideBar {
    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, _ctx: &LayoutCtx) {
        self.bounds = layout.sidebar.outer.into();
    }
}

impl UiComponent for SideBar {
    fn layout(&mut self, ctx: &LayoutCtx) {
        self.bounds = ctx.screen.into();
    }

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::SideBar,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None
    }

    fn render(&self, _ctx: &mut RenderCtx) -> Result<(), GlesError> {
        // existing or temporary no-op
        Ok(())
    }
}
