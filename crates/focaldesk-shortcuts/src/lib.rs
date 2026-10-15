//! Keyboard shortcut policy for the FocalDesk desktop.

mod actions;
pub mod keybinds;

pub use actions::KeyAction;
pub use keybinds::{KeyCombo, KeySymbol, Keybinds, ModMask, ShortcutCapabilities};
