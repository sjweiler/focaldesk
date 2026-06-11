mod state;
mod events;
mod actions;

pub type WindowId = u64;

pub use state::FocusShell;
pub use events::FlowEvent;
pub use actions::FlowAction;
