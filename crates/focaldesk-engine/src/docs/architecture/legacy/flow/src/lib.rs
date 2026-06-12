mod state;
mod events;
mod actions;

pub type WindowId = u64;

pub use state::FocalDesk;
pub use events::FlowEvent;
pub use actions::FlowAction;
