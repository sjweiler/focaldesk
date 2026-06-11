use anyhow::Result;
use flowstate_logging::{flog_info, logging};

#[cfg(feature = "drm")]
use flowstate_engine::backend::drm;

#[cfg(feature = "winit")]
use flowstate_engine::backend::winit;

#[cfg(feature = "drm")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init_default_logging();
    flog_info!("Starting FocusShell");

    drm::run()
}

#[cfg(all(not(feature = "drm"), feature = "winit"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init_default_logging();
    flog_info!("Starting FocusShell");

    winit::run()
}
