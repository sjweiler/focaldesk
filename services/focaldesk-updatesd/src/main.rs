use anyhow::Result;
use focaldesk_ipc::{NotificationIpcRequest, send_notification_request, serve_update_ipc};
use focaldesk_logging::flog_info;
use focaldesk_updates::{UpdateManager, UpdateSnapshot};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    flog_info!("FocalDesk updates daemon starting...");
    let manager = Arc::new(UpdateManager::new());
    serve_update_ipc(Arc::clone(&manager));

    let notifier = Arc::clone(&manager);
    thread::Builder::new()
        .name("focaldesk-updates-notify".into())
        .spawn(move || notify_loop(notifier))?;

    let _ = manager.request_refresh(false);
    loop {
        thread::sleep(Duration::from_secs(60 * 60));
        let snapshot = manager.snapshot();
        if !snapshot.checking && !snapshot.installing {
            let _ = manager.request_refresh(false);
        }
    }
}

fn notify_loop(manager: Arc<UpdateManager>) {
    let initial = manager.snapshot();
    let mut last_count = initial.available_count();
    let mut last_installing = false;
    let mut last_error = initial.last_error;
    loop {
        thread::sleep(Duration::from_secs(2));
        let snapshot = manager.snapshot();
        maybe_notify_available(&snapshot, last_count);
        maybe_notify_error(&snapshot, last_error.as_deref());
        maybe_notify_install(&snapshot, last_installing);
        last_count = snapshot.available_count();
        last_installing = snapshot.installing;
        last_error = snapshot.last_error.clone();
    }
}

fn maybe_notify_available(snapshot: &UpdateSnapshot, last_count: usize) {
    let count = snapshot.available_count();
    if count == 0 || count <= last_count || snapshot.checking || snapshot.installing {
        return;
    }
    let title = if count == 1 {
        "System update available".to_string()
    } else {
        format!("{count} system updates available")
    };
    let body = snapshot
        .packages
        .iter()
        .take(4)
        .map(|package| package.display_title())
        .collect::<Vec<_>>()
        .join(", ");
    let body = if snapshot.packages.len() > 4 {
        format!("{body}, …")
    } else {
        body
    };
    let _ = send_notification_request(&NotificationIpcRequest::Notify {
        title,
        body,
        timeout_ms: Some(8_000),
    });
}

fn maybe_notify_install(snapshot: &UpdateSnapshot, last_installing: bool) {
    if last_installing && !snapshot.installing && snapshot.last_error.is_none() {
        let remaining = snapshot.available_count();
        let body = if remaining == 0 {
            "All selected updates were installed.".to_string()
        } else {
            format!("{remaining} update(s) still available.")
        };
        let _ = send_notification_request(&NotificationIpcRequest::Notify {
            title: "Updates installed".into(),
            body,
            timeout_ms: Some(6_000),
        });
    }
}

fn maybe_notify_error(snapshot: &UpdateSnapshot, previous_error: Option<&str>) {
    let Some(error) = snapshot.last_error.as_deref() else {
        return;
    };
    if previous_error == Some(error) {
        return;
    }
    let _ = send_notification_request(&NotificationIpcRequest::Notify {
        title: "System update error".into(),
        body: error.to_string(),
        timeout_ms: Some(10_000),
    });
}
