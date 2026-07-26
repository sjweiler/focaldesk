use focaldesk_notifications::NotificationSnapshot;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use focaldesk_notifications::NotificationManager;

use crate::transport;

pub const NOTIFICATIONS_SOCKET_NAME: &str = "notifications.sock";
pub const NOTIFICATIONS_SOCKET_ENV: &str = "FOCALDESK_NOTIFICATIONS_SOCKET_PATH";

pub fn notifications_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(NOTIFICATIONS_SOCKET_ENV, NOTIFICATIONS_SOCKET_NAME)
}
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
    let path = notifications_socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|err| format!("could not connect to {}: {err}", path.display()))?;
    transport::configure_stream(&stream).map_err(|err| err.to_string())?;
    let json = transport::encode_message(request)?;

    stream.write_all(&json).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    transport::decode_message(response.as_bytes())
}

pub fn serve_notification_ipc(manager: Arc<std::sync::Mutex<NotificationManager>>) {
    let path =
        notifications_socket_path().expect("could not resolve FocalDesk notifications IPC socket");
    let listener = transport::bind_user_socket(&path)
        .expect("failed to bind FocalDesk notifications IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            handle_notification_client(&mut stream, &manager);
        }
    });
}

fn handle_notification_client(
    stream: &mut UnixStream,
    manager: &Arc<std::sync::Mutex<NotificationManager>>,
) {
    if transport::require_authorized_peer(stream, transport::NOTIFICATIONS_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<NotificationIpcRequest>(&buf) {
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

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}
