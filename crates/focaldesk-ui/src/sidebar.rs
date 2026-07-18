use crate::chrome_layout::ChromeLayout;
use crate::element::{ChromeItem, UiElement, UiRect};
use crate::types::UiElementKind;
use crate::types::{PanelKind, UiAction};
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
    pub const OVERFLOW_ID: u32 = 199_998;
    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, _ctx: &LayoutCtx) {
        self.bounds = layout.sidebar.outer.into();
    }

    pub fn layout_items(
        layout: &ChromeLayout,
        items: impl IntoIterator<Item = ChromeItem>,
    ) -> Vec<UiElement> {
        let capacity = layout.sidebar.slots.len();
        let mut visible: Vec<_> = items.into_iter().filter(|item| item.visible).collect();
        if visible.len() > capacity && capacity > 0 {
            visible.truncate(capacity);
            visible[capacity - 1] = ChromeItem::new(
                Self::OVERFLOW_ID,
                crate::atlas::IconId::Overflow,
                "More sidebar items · open Settings",
                UiAction::OpenPanel(PanelKind::Settings),
            );
        }

        layout
            .sidebar
            .slots
            .iter()
            .zip(visible)
            .map(|(slot, item)| {
                let mut element = UiElement::from_chrome_item(
                    UiElementKind::SidebarButton,
                    item,
                    UiRect::from(slot.outer),
                );
                element.hover_scale = 1.10;
                element.press_scale = 0.96;
                element
            })
            .collect()
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
