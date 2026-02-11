use crate::{FlowAction, FlowEvent, WindowId};

#[derive(Default)]
pub struct FlowState {
    focused: Option<WindowId>,
}

impl FlowState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(&mut self, event: FlowEvent) -> FlowAction {
        match event {
            FlowEvent::FocusChanged { id } => {
                self.focused = id;
                FlowAction::None
            }

            FlowEvent::Key { combo } => {
                if combo == "Mod+Q" {
                    FlowAction::Quit
                } else {
                    FlowAction::None
                }
            }

            _ => FlowAction::None,
        }
    }

    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }
}
