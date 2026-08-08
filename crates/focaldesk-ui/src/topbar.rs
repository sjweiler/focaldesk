use crate::chrome_draw::{draw_chrome_icons_for_elements, draw_chrome_topbar_frame};
use crate::chrome_layout::ChromeLayout;
use crate::clock::ClockComponent;
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

pub struct SystemPanelMeta {
    pub title: String,
    pub application_name: Option<String>,

    pub show_output_label: bool,
    pub output_id: usize,

    pub show_workspace_label: bool,
    pub workspace_id: usize,
}

pub struct SystemPanel {
    pub title: String,
    pub meta: SystemPanelMeta,
    pub indicators: Vec<UiElement>,
    pub clock: ClockComponent,
    pub bounds: UiRect,
    pub elements: Vec<UiElement>,
}

impl Default for SystemPanel {
    fn default() -> Self {
        Self {
            title: "FOCALDESK".into(),
            meta: SystemPanelMeta {
                title: "FOCALDESK".into(),
                application_name: None,
                show_output_label: true,
                output_id: 0,
                show_workspace_label: true,
                workspace_id: 0,
            },
            indicators: Vec::new(),
            clock: ClockComponent::default(),
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

impl SystemPanel {
    pub const OVERFLOW_ID: u32 = 199_999;
    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, ctx: &LayoutCtx) {
        self.bounds = layout.topbar.outer.into();
        self.clock.bounds = layout.topbar.clock_well.into();
        let _ = ctx;
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

    pub fn layout_status_items(
        layout: &ChromeLayout,
        items: impl IntoIterator<Item = ChromeItem>,
    ) -> Vec<UiElement> {
        let capacity = layout.topbar.status_wells.len();
        let mut visible: Vec<_> = items.into_iter().filter(|item| item.visible).collect();
        if visible.len() > capacity && capacity > 0 {
            visible.truncate(capacity);
            visible[capacity - 1] = ChromeItem::new(
                Self::OVERFLOW_ID,
                crate::atlas::IconId::Overflow,
                "More status items · open Settings",
                UiAction::OpenPanel(PanelKind::Settings),
            );
        }

        layout
            .topbar
            .status_wells
            .iter()
            .zip(visible)
            .map(|(well, item)| {
                let mut element = UiElement::from_chrome_item(
                    UiElementKind::TopbarIndicator,
                    item,
                    UiRect::from(*well),
                );
                element.hover_scale = 1.08;
                element.press_scale = 0.96;
                element
            })
            .collect()
    }
}

impl UiComponent for SystemPanel {
    fn layout(&mut self, ctx: &LayoutCtx) {
        self.bounds = ctx.screen.into();
    }

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::SystemPanel,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None
    }

    fn render(&self, ctx: &mut RenderCtx) -> Result<(), GlesError> {
        draw_chrome_topbar_frame(
            ctx.frame,
            ctx.shaders,
            ctx.frame_ctx,
            ctx.chrome_layout,
            &ctx.theme.chrome,
        );
        Ok(())
    }
}

impl SystemPanel {
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
