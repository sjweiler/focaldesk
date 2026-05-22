use crate::element::UiElement;
use smithay::utils::Rectangle;
use smithay::utils::Logical;
use flowstate_types::WidgetId;


use smithay::utils::Point;


pub type UiRect = Rectangle<i32, Logical>;
pub type UiPoint = Point<i32, Logical>;

#[derive(Debug, Clone, Copy)]
pub struct UiHit {
    pub target: UiHitTarget,
    pub widget_id: WidgetId,
    pub point: Point<i32, Logical>,
}

#[derive(Debug, Clone, Copy)]
pub enum UiHitTarget {
    TopBar,
    SideBar,
    WorkArea,
    Dialog,
    Overlay,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutCtx {
    pub screen: UiRect,
    pub scale: f32,
}

pub struct RenderCtx;

pub trait UiComponent {
    fn layout(&mut self, ctx: &LayoutCtx);

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit>;

    fn render(&self, ctx: &mut RenderCtx);
}
