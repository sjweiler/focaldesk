use crate::chrome_layout::ChromeLayout;
use crate::clock::ClockComponent;
use crate::element::UiElement;
use crate::element::UiRect;
use crate::uicomponent::LayoutCtx;
use crate::uicomponent::RenderCtx;
use crate::uicomponent::UiComponent;
use crate::uicomponent::UiHit;
use crate::uicomponent::UiHitTarget;
use flowstate_types::WidgetId;
use smithay::utils::Logical;
use smithay::utils::Point;



pub struct TopBarMeta {
    pub title: String,
    pub application_name: Option<String>,

    pub show_output_label: bool,
    pub output_id: usize,

    pub show_workspace_label: bool,
    pub workspace_id: usize,
}

pub struct TopBar {
    pub title: String,
    pub meta: TopBarMeta,
    pub indicators: Vec<UiElement>,
    pub clock: ClockComponent,
    pub bounds: UiRect,
    pub elements: Vec<UiElement>,
}

impl Default for TopBar {
    fn default() -> Self {
        Self {
            title: "FLOWSTATE".into(),
            meta: TopBarMeta {
                title: "FLOWSTATE".into(),
                application_name: None,
                show_output_label: true,
                output_id: 0,
                show_workspace_label: true,
                workspace_id: 0,
            },
            indicators: Vec::new(),
            clock: ClockComponent::default(),
            bounds: UiRect { x: 0, y: 0, w: 1, h: 1 },
            elements: Vec::new(),
        }
    }
}

impl TopBar {
    pub fn layout_from_chrome(&mut self, layout: &ChromeLayout, ctx: &LayoutCtx) {
        self.bounds = layout.topbar.outer.into();
        self.clock.bounds = layout.topbar.clock_well.into();
        let _ = ctx;
    }
}

impl UiComponent for TopBar {
    fn layout(&mut self, ctx: &LayoutCtx) {
        self.bounds = ctx.screen.into();
    }
    
    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit>
    {
       for element in self.elements.iter().rev() {
            if element.bounds.contains(point.x, point.y) {
                return Some(UiHit {
                    target: UiHitTarget::TopBar,
                    widget_id: WidgetId(element.id),
                    point,
                });
            }
        }

        None 
    }

    fn render(&self, renderer: &mut RenderCtx) {
        self.clock.render(renderer);

        // Later: render topbar background, title, meta text, indicators.
    }   

}


