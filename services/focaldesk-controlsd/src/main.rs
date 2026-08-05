use anyhow::{Result, anyhow};
use focaldesk_ipc::{
    ControlIpcRequest, ControlIpcResponse, ControlSetting, NotificationIpcRequest,
    NotificationIpcResponse, send_notification_request, serve_control_ipc,
};
use focaldesk_logging::flog_info;
use std::process::Command;
use std::sync::Arc;

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let handler = Arc::new(move |request: ControlIpcRequest| -> ControlIpcResponse {
        match request {
            ControlIpcRequest::SetSystemSetting { setting, enabled } => {
                match set_system_setting(setting, enabled) {
                    Ok(()) => ControlIpcResponse::Ok,
                    Err(message) => ControlIpcResponse::Error { message },
                }
            }
            ControlIpcRequest::SetVolume { volume } => match set_default_audio_volume(volume) {
                Ok(()) => ControlIpcResponse::Ok,
                Err(message) => ControlIpcResponse::Error { message },
            },
        }
    });

    flog_info!("FocalDesk control daemon starting...");
    serve_control_ipc(handler);
    std::thread::park();
    Ok(())
}

fn set_system_setting(setting: ControlSetting, enabled: bool) -> Result<(), String> {
    match setting {
        ControlSetting::Wifi => {
            let state = if enabled { "on" } else { "off" };
            run_command("nmcli", &["radio", "wifi", state])
        }
        ControlSetting::Bluetooth => focaldesk_bluetooth::set_power(enabled).map(|_| ()),
        ControlSetting::DoNotDisturb => {
            match send_notification_request(&NotificationIpcRequest::SetDoNotDisturb { enabled }) {
                Ok(NotificationIpcResponse::Ok) => Ok(()),
                Ok(NotificationIpcResponse::Error { message }) => Err(message),
                Ok(other) => Err(format!("unexpected notification response: {other:?}")),
                Err(err) => Err(err),
            }
        }
    }
}

fn set_default_audio_volume(volume: f32) -> Result<(), String> {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round();
    let percent = format!("{percent:.0}%");
    run_command(
        "wpctl",
        &["set-volume", "@DEFAULT_AUDIO_SINK@", percent.as_str()],
    )
}

fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|err| err.to_string())?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{program} exited with {status}").to_string())
    }
}
