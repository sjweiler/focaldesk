use focaldesk_power::{PowerManager, PowerSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    thread,
};

pub const POWER_SOCKET_PATH: &str = "/tmp/focaldesk-power.sock";

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
    let mut stream = UnixStream::connect(POWER_SOCKET_PATH)
        .map_err(|err| format!("could not connect to {POWER_SOCKET_PATH}: {err}"))?;
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

pub fn serve_power_ipc(manager: Arc<PowerManager>) {
    let _ = std::fs::remove_file(POWER_SOCKET_PATH);

    let listener =
        UnixListener::bind(POWER_SOCKET_PATH).expect("failed to bind FocalDesk power IPC socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                handle_power_client(&mut stream, &manager);
            }
        }
    });
}

fn handle_power_client(stream: &mut UnixStream, manager: &Arc<PowerManager>) {
    let mut buf = String::new();

    if stream.read_to_string(&mut buf).is_err() {
        return;
    }

    let response = match serde_json::from_str::<PowerIpcRequest>(&buf) {
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

    let json = serde_json::to_string(&response).unwrap();
    let _ = stream.write_all(json.as_bytes());
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
