use crate::chrome_draw::{draw_chrome_icons_for_elements, draw_chrome_sidebar_frame};
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

pub struct Dock {
    pub buttons: Vec<UiElement>,
    pub workspace_buttons: Vec<UiElement>,
    pub bounds: UiRect,
    pub elements: Vec<UiElement>,
}

impl Default for Dock {
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

impl Dock {
    pub const OVERFLOW_ID: u32 = 199_998;
    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, _ctx: &LayoutCtx) {
        self.bounds = layout.sidebar.outer.into();
    }

    pub fn set_elements(&mut self, elements: Vec<UiElement>) {
        self.elements = elements;
    }

    pub fn update_hover(&mut self, point: Point<i32, Logical>) -> bool {
        let mut changed = false;
        for element in &mut self.elements {
            let hovered =
                element.visible && element.enabled && element.bounds.contains(point.x, point.y);
            changed |= element.hovered != hovered;
            element.hovered = hovered;
        }
        changed
    }

    pub fn clear_hover(&mut self) -> bool {
        let mut changed = false;
        for element in &mut self.elements {
            changed |= element.hovered;
            element.hovered = false;
        }
        changed
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

impl UiComponent for Dock {
    fn layout(&mut self, ctx: &LayoutCtx) {
        self.bounds = ctx.screen.into();
    }

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::Dock,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None
    }

    fn render(&self, ctx: &mut RenderCtx) -> Result<(), GlesError> {
        draw_chrome_sidebar_frame(
            ctx.frame,
            ctx.shaders,
            ctx.frame_ctx,
            ctx.chrome_layout,
            self.elements.iter().position(|element| element.hovered),
            &ctx.theme.chrome,
        );
        Ok(())
    }
}

impl Dock {
    pub fn render_icons(&self, ctx: &mut RenderCtx) -> Result<(), GlesError> {
        draw_chrome_icons_for_elements(
            ctx.frame,
            ctx.shaders,
            ctx.frame_ctx,
            ctx.chrome_layout,
            &self.elements,
            ctx.theme,
            ctx.atlas,
            ctx.metrics,
        )
    }
}
