//! FlowState compositor configuration (TOML on disk).

mod config;

pub use config::{load_config, save_config, FlowStateConfig};
