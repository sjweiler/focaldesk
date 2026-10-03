pub mod client;
pub mod constants;
pub mod errors;
pub mod policy;
pub mod protocol;

pub use client::request_launch;
pub use constants::{DEFAULT_TIMEOUT_MS, SOCKET_BASENAME, SOCKET_ENV, socket_path};
pub use errors::{LaunchError, Result};
pub use policy::{
    auto_hdr_app_enabled, auto_hdr_gamut_wideness, auto_hdr_sdr_nits, auto_hdr_target_nits,
    chrome_command_args, chrome_hdr_mode_active, is_browser_like, is_chrome_like,
};
pub use protocol::{BrowserBackend, LaunchRequest, LaunchResponse, LaunchSource};
