use anyhow::Result;
use focaldesk_logging::{init_default_logging, session_id, startup_banner};
use tracing::info;

#[cfg(feature = "drm")]
use focaldesk_engine::backend::drm;
#[cfg(all(not(feature = "drm"), feature = "winit"))]
use focaldesk_engine::backend::winit;
#[cfg(feature = "drm")]
use std::path::PathBuf;
#[cfg(feature = "drm")]
use std::process::{Child, Command, Stdio};

#[cfg(feature = "drm")]
fn spawn_polkit_agent() -> std::io::Result<Child> {
    let executable = std::env::var_os("FOCALDESK_POLKIT_AGENT")
        .map(PathBuf::from)
        .or_else(|| {
            let mut candidates = vec![
                "/usr/libexec/focaldesk/focaldesk-polkitd",
                "/usr/bin/focaldesk-polkitd",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
            if let Some(home) = std::env::var_os("HOME") {
                candidates.push(PathBuf::from(home).join(".local/bin/focaldesk-polkitd"));
            }
            candidates.into_iter().find(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("focaldesk-polkitd"));

    Command::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
}

#[cfg(feature = "drm")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_default_logging();
    startup_banner(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), "drm");
    info!(target: "focaldesk", session_id = session_id(), backend = "drm", "starting FocalDesk");
    // A PolicyKit agent must register from the graphical login session. A
    // systemd --user service belongs to user@.service instead, and polkit
    // rejects it because the caller and registered sessions differ.
    let mut polkit_agent = spawn_polkit_agent()?;
    let result = drm::run();
    let _ = polkit_agent.kill();
    let _ = polkit_agent.wait();
    result
}

#[cfg(all(not(feature = "drm"), feature = "winit"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_default_logging();
    startup_banner(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), "winit");
    info!(target: "focaldesk", session_id = session_id(), backend = "winit", "starting FocalDesk");
    winit::run()
}
