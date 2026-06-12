use crate::chrome_shaders::ChromeShaders;
use crate::desktop_frame::DesktopFrameCtx;
use crate::dialog::DialogId;
use focaldesk_themes::FlowTheme;
use focaldesk_types::OutputId;
use focaldesk_types::WidgetId;
use smithay::backend::renderer::gles::GlesError;
use smithay::backend::renderer::gles::GlesFrame;
use smithay::utils::Logical;
use smithay::utils::Physical;
use smithay::utils::Rectangle;

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

pub struct RenderCtx<'a, 'b> {
    pub frame: &'a mut GlesFrame<'b, 'b>,
    pub frame_ctx: &'a DesktopFrameCtx,
    pub damage: &'a [Rectangle<i32, Physical>],

    pub output_scale: f64,
    pub output_id: OutputId,

    // NEW
    pub shaders: &'a ChromeShaders,
    pub theme: &'a FlowTheme,
    pub active_dialog: Option<DialogId>,
    pub draw_on_this_output: bool,
}

pub trait UiComponent {
    fn layout(&mut self, ctx: &LayoutCtx);

    fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit>;

    fn render(&self, ctx: &mut RenderCtx) -> Result<(), GlesError>;
}
