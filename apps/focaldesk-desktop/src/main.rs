use anyhow::Result;
use focaldesk_logging::{init_default_logging, session_id, startup_banner};
use std::process::Command;
use tracing::info;

#[cfg(feature = "drm")]
use focaldesk_engine::backend::drm;
use std::process::Stdio;

#[cfg(all(not(feature = "drm"), feature = "winit"))]
use focaldesk_engine::backend::winit;

#[cfg(feature = "drm")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_default_logging();
    startup_banner(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), "drm");
    info!(target: "focaldesk", session_id = session_id(), backend = "drm", "starting FocalDesk");
    Command::new("/usr/libexec/focaldesk/focaldesk-polkitd")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    drm::run()
}

#[cfg(all(not(feature = "drm"), feature = "winit"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_default_logging();
    startup_banner(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), "winit");
    info!(target: "focaldesk", session_id = session_id(), backend = "winit", "starting FocalDesk");
    winit::run()
}
