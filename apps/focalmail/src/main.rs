#![cfg_attr(not(feature = "desktop"), allow(dead_code))]
mod attachments;
mod bulk;
mod drafts;
#[path = "gmail.rs"]
mod gmail_backend;
mod html;
mod html_layout;
mod imap_backend;
mod message;
mod mime;
mod notifications;
mod outlook;
mod providers;
mod remote_images;
mod service;
mod settings;
mod store;
mod sync_recovery;
use message::Message;
#[cfg(feature = "desktop")]
mod desktop;
#[cfg(test)]
mod fixtures;

fn main() -> anyhow::Result<()> {
    if let Some(result) = service::command_line() {
        return result;
    }
    #[cfg(feature = "desktop")]
    {
        desktop::run()
    }
    #[cfg(not(feature = "desktop"))]
    {
        anyhow::bail!("This build supports --mail-service and --mail-request only; enable the desktop feature for the Tauri window")
    }
}
