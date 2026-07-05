use focaldesk_notifications::NotificationSnapshot;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use focaldesk_notifications::NotificationManager;

pub const NOTIFICATIONS_SOCKET_PATH: &str = "/tmp/focaldesk-notifications.sock";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NotificationIpcRequest {
    Notify {
        title: String,
        body: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    GetVisible,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum NotificationIpcResponse {
    Ok,
    NotificationQueued {
        id: u64,
    },
    VisibleNotifications {
        notifications: Vec<NotificationSnapshot>,
    },
    Error {
        message: String,
    },
}

pub fn send_notification_request(
    request: &NotificationIpcRequest,
) -> Result<NotificationIpcResponse, String> {
    let mut stream = UnixStream::connect(NOTIFICATIONS_SOCKET_PATH)
        .map_err(|err| format!("could not connect to {NOTIFICATIONS_SOCKET_PATH}: {err}"))?;
    let json = serde_json::to_vec(request).map_err(|err| err.to_string())?;

    stream.write_all(&json).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    serde_json::from_str(&response).map_err(|err| err.to_string())
}

pub fn serve_notification_ipc(manager: Arc<std::sync::Mutex<NotificationManager>>) {
    let _ = std::fs::remove_file(NOTIFICATIONS_SOCKET_PATH);

    let listener = UnixListener::bind(NOTIFICATIONS_SOCKET_PATH)
        .expect("failed to bind FocalDesk notifications IPC socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                handle_notification_client(&mut stream, &manager);
            }
        }
    });
}

fn handle_notification_client(
    stream: &mut UnixStream,
    manager: &Arc<std::sync::Mutex<NotificationManager>>,
) {
    let mut buf = String::new();

    if stream.read_to_string(&mut buf).is_err() {
        return;
    }

    let response = match serde_json::from_str::<NotificationIpcRequest>(&buf) {
        Ok(NotificationIpcRequest::Notify {
            title,
            body,
            timeout_ms,
        }) => {
            let timeout = timeout_ms.map(Duration::from_millis);
            let id = {
                let mut manager = manager.lock().unwrap();
                manager.push_with_timeout(title, body, timeout)
            };

            NotificationIpcResponse::NotificationQueued { id }
        }
        Ok(NotificationIpcRequest::GetVisible) => {
            let notifications = {
                let mut manager = manager.lock().unwrap();
                let now = Instant::now();
                let _ = manager.expire(now);
                manager.visible_snapshots(now)
            };

            NotificationIpcResponse::VisibleNotifications { notifications }
        }
        Err(err) => NotificationIpcResponse::Error {
            message: err.to_string(),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let _ = stream.write_all(json.as_bytes());
}
