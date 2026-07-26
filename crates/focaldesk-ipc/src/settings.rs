use crate::{IpcRequest, IpcResponse, settings_socket_path, transport};
use focaldesk_settings_core::{
    BrowserLaunchBackend, DebugLogLevel, LidCloseAction, LowBatteryAction, PerformanceMode,
    PowerButtonAction, Settings, load_settings, save_settings,
};
use std::{
    io::Write,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    thread,
};

pub fn serve_settings_ipc(settings: Arc<Mutex<Settings>>) {
    let path = settings_socket_path().expect("could not resolve FocalDesk settings IPC socket");
    let listener =
        transport::bind_user_socket(&path).expect("failed to bind FocalDesk settings IPC socket");

    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            handle_settings_client(&mut stream, &settings);
        }
    });
}

fn handle_settings_client(stream: &mut UnixStream, settings: &Arc<Mutex<Settings>>) {
    if transport::require_authorized_peer(stream, transport::SETTINGS_POLICY).is_err() {
        return;
    }
    let Ok(buf) = transport::read_limited(stream) else {
        return;
    };

    let response = match transport::decode_message::<IpcRequest>(&buf) {
        Ok(IpcRequest::GetAll) => {
            let settings = settings.lock().unwrap().clone();
            IpcResponse::Settings { settings }
        }

        Ok(IpcRequest::GetPowerSnapshot) => IpcResponse::Error {
            message: "request is handled by focaldesk-desktop".to_string(),
        },

        Ok(IpcRequest::SetDisplays { outputs }) => {
            let mut s = settings.lock().unwrap();
            s.displays.outputs = outputs;

            match save_settings(&s) {
                Ok(_) => IpcResponse::Ok,
                Err(e) => IpcResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        Ok(IpcRequest::SetValue { path, value }) => {
            let mut s = settings.lock().unwrap();

            match apply_setting_value(&mut s, &path, value) {
                Ok(_) => match save_settings(&s) {
                    Ok(_) => IpcResponse::Ok,
                    Err(e) => IpcResponse::Error {
                        message: e.to_string(),
                    },
                },
                Err(e) => IpcResponse::Error { message: e },
            }
        }

        Ok(IpcRequest::IdentifyDisplays) => {
            // Set a compositor flag here:
            // state.identify_outputs_until = Some(Instant::now() + Duration::from_secs(3));
            IpcResponse::Ok
        }

        Ok(IpcRequest::Reload) => {
            let loaded = load_settings();
            *settings.lock().unwrap() = loaded;
            IpcResponse::Ok
        }

        Ok(IpcRequest::ReloadConfig) => IpcResponse::Ok,

        Ok(
            IpcRequest::Get { .. }
            | IpcRequest::Set { .. }
            | IpcRequest::Watch { .. }
            | IpcRequest::GetConfig
            | IpcRequest::SetConfig { .. }
            | IpcRequest::GetDisplayRuntimeStatus
            | IpcRequest::Notify { .. }
            | IpcRequest::ExecuteDesktopAction { .. },
        ) => IpcResponse::Error {
            message: "request is handled by focaldesk-desktop".to_string(),
        },

        Err(e) => IpcResponse::Error {
            message: e.to_string(),
        },
    };

    if let Ok(json) = transport::encode_message(&response) {
        let _ = stream.write_all(&json);
    }
}

fn apply_setting_value(
    settings: &mut Settings,
    path: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    match path {
        "appearance.theme" => {
            settings.appearance.theme = value.as_str().ok_or("theme must be string")?.to_string();
        }

        "appearance.sidebar_width" => {
            settings.appearance.sidebar_width =
                value.as_i64().ok_or("sidebar_width must be integer")? as i32;
        }

        "appearance.topbar_height" => {
            settings.appearance.topbar_height =
                value.as_i64().ok_or("topbar_height must be integer")? as i32;
        }

        "appearance.icon_size" => {
            settings.appearance.icon_size =
                value.as_i64().ok_or("icon_size must be integer")? as i32;
        }

        "appearance.animations" => {
            settings.appearance.animations = value.as_bool().ok_or("animations must be bool")?;
        }

        "input.pointer_speed" => {
            settings.input.pointer_speed =
                value.as_f64().ok_or("pointer_speed must be number")? as f32;
        }

        "input.natural_scroll" => {
            settings.input.natural_scroll = value.as_bool().ok_or("natural_scroll must be bool")?;
        }

        "apps.terminal" => {
            settings.apps.terminal = value.as_str().ok_or("terminal must be string")?.to_string();
        }

        "apps.browser" => {
            settings.apps.browser = value.as_str().ok_or("browser must be string")?.to_string();
        }

        "apps.browser_launch_backend" => {
            settings.apps.browser_launch_backend = match value
                .as_str()
                .ok_or("browser_launch_backend must be string")?
            {
                "auto" => BrowserLaunchBackend::Auto,
                "wayland" => BrowserLaunchBackend::Wayland,
                "xwayland" => BrowserLaunchBackend::Xwayland,
                other => return Err(format!("unknown browser_launch_backend: {other}")),
            };
        }

        "apps.file_manager" => {
            settings.apps.file_manager = value
                .as_str()
                .ok_or("file_manager must be string")?
                .to_string();
        }

        "privacy.recent_files" => {
            settings.privacy.recent_files = value.as_bool().ok_or("recent_files must be bool")?;
        }

        "privacy.location_services" => {
            settings.privacy.location_services =
                value.as_bool().ok_or("location_services must be bool")?;
        }

        "privacy.hide_lock_screen_notifications" => {
            settings.privacy.hide_lock_screen_notifications = value
                .as_bool()
                .ok_or("hide_lock_screen_notifications must be bool")?;
        }

        "power.blank_screen_minutes" => {
            settings.power.blank_screen_minutes = optional_minutes(value)?;
        }

        "power.suspend_minutes" => {
            settings.power.suspend_minutes = optional_minutes(value)?;
        }

        "power.power_button_action" => {
            settings.power.power_button_action =
                match value.as_str().ok_or("power_button_action must be string")? {
                    "show_power_menu" => PowerButtonAction::ShowPowerMenu,
                    "suspend" => PowerButtonAction::Suspend,
                    "power_off" => PowerButtonAction::PowerOff,
                    "do_nothing" => PowerButtonAction::DoNothing,
                    other => return Err(format!("unknown power button action: {other}")),
                };
        }

        "power.lid_close_action" => {
            settings.power.lid_close_action =
                match value.as_str().ok_or("lid_close_action must be string")? {
                    "suspend" => LidCloseAction::Suspend,
                    "blank_screen" => LidCloseAction::BlankScreen,
                    "lock_screen" => LidCloseAction::LockScreen,
                    "do_nothing" => LidCloseAction::DoNothing,
                    other => return Err(format!("unknown lid close action: {other}")),
                };
        }

        "power.low_battery_action" => {
            settings.power.low_battery_action =
                match value.as_str().ok_or("low_battery_action must be string")? {
                    "notify_only" => LowBatteryAction::NotifyOnly,
                    "suspend" => LowBatteryAction::Suspend,
                    "hibernate" => LowBatteryAction::Hibernate,
                    "power_off" => LowBatteryAction::PowerOff,
                    other => return Err(format!("unknown low battery action: {other}")),
                };
        }

        "power.performance_mode" => {
            settings.power.performance_mode =
                match value.as_str().ok_or("performance_mode must be string")? {
                    "balanced" => PerformanceMode::Balanced,
                    "performance" => PerformanceMode::Performance,
                    "power_saver" => PerformanceMode::PowerSaver,
                    other => return Err(format!("unknown performance mode: {other}")),
                };
        }

        "workspaces.restore_session" => {
            settings.workspaces.restore_session =
                value.as_bool().ok_or("restore_session must be bool")?;
        }

        "workspaces.maximize_on_launch" => {
            settings.workspaces.maximize_on_launch =
                value.as_bool().ok_or("maximize_on_launch must be bool")?;
        }

        "debug.log_level" => {
            settings.debug.log_level = match value.as_str().ok_or("log_level must be string")? {
                "error" => DebugLogLevel::Error,
                "warn" => DebugLogLevel::Warn,
                "info" => DebugLogLevel::Info,
                "debug" => DebugLogLevel::Debug,
                "trace" => DebugLogLevel::Trace,
                other => return Err(format!("unknown debug log level: {other}")),
            };
        }

        "debug.show_fps" => {
            settings.debug.show_fps = value.as_bool().ok_or("show_fps must be bool")?;
        }

        "debug.show_damage_regions" => {
            settings.debug.show_damage_regions =
                value.as_bool().ok_or("show_damage_regions must be bool")?;
        }

        "debug.show_input_events" => {
            settings.debug.show_input_events =
                value.as_bool().ok_or("show_input_events must be bool")?;
        }

        "debug.verbose_protocol_logs" => {
            settings.debug.verbose_protocol_logs = value
                .as_bool()
                .ok_or("verbose_protocol_logs must be bool")?;
        }

        _ => return Err(format!("unknown setting path: {path}")),
    }

    Ok(())
}

fn optional_minutes(value: serde_json::Value) -> Result<Option<u32>, String> {
    if value.is_null() {
        return Ok(None);
    }

    let minutes = value.as_u64().ok_or("timeout must be integer or null")?;
    u32::try_from(minutes)
        .map(Some)
        .map_err(|_| "timeout is too large".to_string())
}
