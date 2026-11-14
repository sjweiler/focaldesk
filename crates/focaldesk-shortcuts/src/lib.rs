//! Keyboard shortcut policy for the FocalDesk desktop.

mod actions;
mod catalog;
pub use catalog::EDITABLE_SHORTCUTS;
pub mod keybinds;

pub use actions::KeyAction;
pub use keybinds::{KeyCombo, KeySymbol, Keybinds, ModMask, ShortcutCapabilities, parse_shortcut};
