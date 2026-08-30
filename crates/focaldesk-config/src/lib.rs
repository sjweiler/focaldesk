//! FocalDesk compositor configuration (TOML on disk).

mod config;

pub use config::{
    configured_theme, load_config, save_config, ClockFormat, DockConfig, DockPosition, DockSize,
    DockVisibility, FocalDeskConfig, PanelConfig, PanelPosition, ShellConfig, ShellStyle,
};
