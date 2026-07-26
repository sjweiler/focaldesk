use focaldesk_power::{PowerManager, PowerSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
};

use crate::transport;

pub const POWER_SOCKET_NAME: &str = "power.sock";
pub const POWER_SOCKET_ENV: &str = "FOCALDESK_POWER_SOCKET_PATH";

pub fn power_socket_path() -> Result<std::path::PathBuf, String> {
    transport::socket_path(POWER_SOCKET_ENV, POWER_SOCKET_NAME)
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PowerIpcRequest {
    GetSnapshot,
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
    SetPerformanceProfile { profile: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum PowerIpcResponse {
    Ok,
    PowerSnapshot { snapshot: PowerSnapshot },
    Error { message: String },
}

pub fn send_power_request(request: &PowerIpcRequest) -> Result<PowerIpcResponse, String> {
    let path = power_socket_path()?;
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

pub fn serve_power_ipc(manager: Arc<PowerManager>) {
    let path = power_socket_path().expect("could not resolve FocalDesk power IPC socket");
    let listener =
        transport::bind_user_socket(&path).expect("failed to bind FocalDesk power IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            handle_power_client(&mut stream, &manager);
        }
    });
}

fn handle_power_client(stream: &mut UnixStream, manager: &Arc<PowerManager>) {
    if transport::require_authorized_peer(stream, transport::POWER_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<PowerIpcRequest>(&buf) {
        Ok(PowerIpcRequest::GetSnapshot) => PowerIpcResponse::PowerSnapshot {
            snapshot: manager.snapshot(),
        },
        Ok(PowerIpcRequest::Suspend) => {
            dispatch_power_command(manager, |manager| manager.suspend())
        }
        Ok(PowerIpcRequest::Hibernate) => {
            dispatch_power_command(manager, |manager| manager.hibernate())
        }
        Ok(PowerIpcRequest::Reboot) => dispatch_power_command(manager, |manager| manager.reboot()),
        Ok(PowerIpcRequest::PowerOff) => {
            dispatch_power_command(manager, |manager| manager.power_off())
        }
        Ok(PowerIpcRequest::SetPerformanceProfile { profile }) => {
            match manager.set_performance_profile(&profile) {
                Ok(()) => PowerIpcResponse::Ok,
                Err(err) => PowerIpcResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        Err(err) => PowerIpcResponse::Error {
            message: err.to_string(),
        },
    };

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}

fn dispatch_power_command(
    manager: &Arc<PowerManager>,
    command: impl FnOnce(&PowerManager) -> Result<(), focaldesk_power::PowerError> + Send + 'static,
) -> PowerIpcResponse {
    let manager = Arc::clone(manager);
    match thread::Builder::new()
        .name("focaldesk-power-action".to_string())
        .spawn(move || {
            if let Err(err) = command(&manager) {
                eprintln!("power action failed: {err}");
            }
        }) {
        Ok(_) => PowerIpcResponse::Ok,
        Err(err) => PowerIpcResponse::Error {
            message: format!("could not dispatch power action: {err}"),
        },
    }
}
