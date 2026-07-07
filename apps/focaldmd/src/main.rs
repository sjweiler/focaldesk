//! focaldmd — Wayland-native display manager daemon.
//!
//! Runs as root from a systemd unit that conflicts with getty@tty1.
//! Supervises the greeter (unprivileged focaldm user), drives PAM, and
//! launches focaldesk as the authenticated user on the same VT.

mod config;
mod ipc;
mod pam;
mod supervise;

use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let cfg = config::Config::load().context("load config")?;

    if !nix::unistd::Uid::effective().is_root() {
        anyhow::bail!("focaldmd must run as root");
    }

    // Single-threaded runtime is plenty: all heavy blocking work (PAM)
    // lives on its own std threads.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(supervise::run(cfg))
}
