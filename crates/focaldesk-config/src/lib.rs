//! FocalDesk compositor configuration (TOML on disk).

mod config;

pub use config::{
    configured_theme, load_config, load_config_read_only, migrate_legacy_config, save_config,
    ClockFormat, DockConfig, DockPosition, DockSize, DockVisibility, FocalDeskConfig, PanelConfig,
    PanelPosition, RailBackdrop, ShellConfig, ShellStyle,
};
