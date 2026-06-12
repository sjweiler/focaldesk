use crate::chrome_layout::ChromeLayout;
use crate::element::UiElement;
use crate::element::UiRect;
use crate::uicomponent::UiHit;
use crate::uicomponent::UiHitTarget;
use crate::uicomponent::{LayoutCtx, RenderCtx, UiComponent};
use focaldesk_types::WidgetId;
use smithay::backend::renderer::gles::GlesError;
use smithay::utils::Logical;
use smithay::utils::Point;

pub struct WorkArea {
    pub bounds: UiRect,
    pub elements: Vec<UiElement>,
}

impl WorkArea {
    pub fn new() -> Self {
        Self {
            bounds: UiRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            elements: Vec::new(),
        }
    }

    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, _ctx: &LayoutCtx) {
        self.bounds = layout.work_area.recess.into();
    }
}

impl UiComponent for WorkArea {
    fn layout(&mut self, ctx: &LayoutCtx) {
        self.bounds = ctx.screen.into();
    }

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::WorkArea,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None
    }

    fn render(&self, _ctx: &mut RenderCtx) -> Result<(), GlesError> {
        // WorkArea probably does not draw anything yet.
        // Client/window rendering still happens elsewhere.
        Ok(())
    }
}
