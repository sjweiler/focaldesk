//! Frontend-independent Settings application APIs. System changes remain in Rust.
pub mod accessibility;
pub mod appearance;
pub mod applications;
pub mod bluetooth;
pub mod calendar;
pub mod chrome;
mod control_process;
pub mod debug;
pub mod displays;
pub mod input;
pub mod navigation;
pub mod network;
pub mod preferences;
pub mod printers;
pub mod sound;
pub mod theme_editor;
pub mod workspaces;

mod location_permissions;
pub mod operations;
