//! FocalDesk compositor configuration (TOML on disk).

mod config;

pub use config::{
    configured_theme, load_config, save_config, DockConfig, DockPosition, DockSize,
    FocalDeskConfig, PanelConfig, PanelPosition, ShellConfig, ShellStyle,
};
