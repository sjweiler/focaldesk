use std::sync::{Arc, Mutex};
use std::thread;

use focaldesk_logging::{init_default_logging, session_id};
use focaldesk_settings_core::load_settings;
use tracing::info;

use focaldesk_ipc::serve_settings_ipc;

fn main() {
    init_default_logging();
    info!(
        target: "focaldesk",
        session_id = session_id(),
        "settings IPC daemon started"
    );

    let settings = Arc::new(Mutex::new(load_settings()));
    serve_settings_ipc(settings);
    thread::park();
}
