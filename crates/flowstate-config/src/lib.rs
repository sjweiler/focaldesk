//! FlowState compositor configuration (TOML on disk).

mod config;

pub use config::{
    FlowStateConfig,
    load_config,
    save_config,
};
