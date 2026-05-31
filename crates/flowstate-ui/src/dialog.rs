// crates/flowstate-ui/src/dialog.rs
use crate::uicomponent::UiHit;
use crate::uicomponent::UiHitTarget;
use flowstate_types::OutputId;
use flowstate_types::WidgetId;
use smithay::utils::Logical;
use smithay::utils::Point;
use smithay::utils::Rectangle;

pub type DialogId = u32;

#[derive(Debug, Clone)]
pub enum DialogKind {
    Info,
    Confirm,
    Destructive,
    Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    Open,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    Confirm,
    Cancel,
    Custom(u32),
}

#[derive(Debug, Clone)]
pub struct DialogButton {
    pub label: String,
    pub action: DialogAction,
}

#[derive(Debug, Clone)]
pub struct Dialog {
    pub id: DialogId,
    pub kind: DialogKind,
    pub title: String,
    pub message: String,
    pub buttons: Vec<DialogButton>,
    pub modal: bool,
    pub dismissible: bool,
    pub state: DialogState,
    pub owner_output: OutputId,
    pub bounds: Rectangle<i32, Logical>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiModal {
    None,
    Dialog(DialogId),
}

impl Dialog {
    pub fn hit_test(&self, point: Point<i32, Logical>) -> Option<UiHit> {
        if self.bounds.contains(point) {
            return Some(UiHit {
                target: UiHitTarget::Dialog,
                widget_id: WidgetId(0),
                point,
            });
        }

        None
    }
    pub fn layout(&mut self) {}
}
