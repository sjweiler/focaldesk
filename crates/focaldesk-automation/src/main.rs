use focaldesk_logging::{init_default_logging, session_id};
use tracing::info;

fn main() {
    init_default_logging();
    info!(
        target: "focaldesk",
        session_id = session_id(),
        "automation daemon started"
    );
}
