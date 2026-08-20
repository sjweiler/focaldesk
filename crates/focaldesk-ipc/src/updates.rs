use focaldesk_updates::{UpdateManager, UpdateSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
};

use crate::transport;

pub const UPDATES_SOCKET_NAME: &str = "updates.sock";
pub const UPDATES_SOCKET_ENV: &str = "FOCALDESK_UPDATES_SOCKET_PATH";

pub fn updates_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(UPDATES_SOCKET_ENV, UPDATES_SOCKET_NAME)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UpdateIpcRequest {
    GetState,
    Refresh {
        #[serde(default)]
        refresh_metadata: bool,
    },
    Install {
        ids: Vec<String>,
    },
    InstallAll,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum UpdateIpcResponse {
    Ok,
    Accepted,
    State { snapshot: UpdateSnapshot },
    Error { message: String },
}

pub fn send_update_request(request: &UpdateIpcRequest) -> Result<UpdateIpcResponse, String> {
    let path = updates_socket_path()?;
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

pub fn serve_update_ipc(manager: Arc<UpdateManager>) {
    let path = updates_socket_path().expect("could not resolve FocalDesk updates IPC socket");
    let listener =
        transport::bind_user_socket(&path).expect("failed to bind FocalDesk updates IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            handle_update_client(&mut stream, &manager);
        }
    });
}

fn handle_update_client(stream: &mut UnixStream, manager: &Arc<UpdateManager>) {
    if transport::require_authorized_peer(stream, transport::UPDATES_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<UpdateIpcRequest>(&buf) {
        Ok(UpdateIpcRequest::GetState) => UpdateIpcResponse::State {
            snapshot: manager.snapshot(),
        },
        Ok(UpdateIpcRequest::Refresh { refresh_metadata }) => {
            match manager.request_refresh(refresh_metadata) {
                Ok(()) => UpdateIpcResponse::Accepted,
                Err(message) => UpdateIpcResponse::Error { message },
            }
        }
        Ok(UpdateIpcRequest::Install { ids }) => match manager.request_install(ids) {
            Ok(()) => UpdateIpcResponse::Accepted,
            Err(message) => UpdateIpcResponse::Error { message },
        },
        Ok(UpdateIpcRequest::InstallAll) => match manager.request_install_all() {
            Ok(()) => UpdateIpcResponse::Accepted,
            Err(message) => UpdateIpcResponse::Error { message },
        },
        Err(err) => UpdateIpcResponse::Error {
            message: err.to_string(),
        },
    };

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}
