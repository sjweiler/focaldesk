// crates/flowstate-ui/src/dialog.rs
use flowstate_types::OutputId;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiModal {
    None,
    Dialog(DialogId),
}
