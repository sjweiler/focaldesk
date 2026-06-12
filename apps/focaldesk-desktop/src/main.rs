use anyhow::Result;
use focaldesk_logging::{flog_info, logging};

#[cfg(feature = "drm")]
use focaldesk_engine::backend::drm;

#[cfg(feature = "winit")]
use focaldesk_engine::backend::winit;

#[cfg(feature = "drm")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init_default_logging();
    flog_info!("Starting FocalDesk");

    drm::run()
}

#[cfg(all(not(feature = "drm"), feature = "winit"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init_default_logging();
    flog_info!("Starting FocalDesk");

    winit::run()
}
