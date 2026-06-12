use focaldesk_ipc::{IpcRequest, IpcResponse, SOCKET_PATH};
use focaldesk_settings_core::{load_settings, save_settings, Settings};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Mutex},
    thread,
};

pub fn start_settings_ipc(settings: Arc<Mutex<Settings>>) {
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH)
        .expect("failed to bind FocalDesk settings IPC socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                handle_settings_client(&mut stream, &settings);
            }
        }
    });
}

fn handle_settings_client(stream: &mut UnixStream, settings: &Arc<Mutex<Settings>>) {
    let mut buf = String::new();

    if stream.read_to_string(&mut buf).is_err() {
        return;
    }

    let response = match serde_json::from_str::<IpcRequest>(&buf) {
        Ok(IpcRequest::GetAll) => {
            let settings = settings.lock().unwrap().clone();
            IpcResponse::Settings { settings }
        }

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

        Err(e) => IpcResponse::Error {
            message: e.to_string(),
        },
    };

    let json = serde_json::to_string(&response).unwrap();
    let _ = stream.write_all(json.as_bytes());
}

fn apply_setting_value(
    settings: &mut Settings,
    path: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    match path {
        "appearance.theme" => {
            settings.appearance.theme = value
                .as_str()
                .ok_or("theme must be string")?
                .to_string();
        }

        "appearance.sidebar_width" => {
            settings.appearance.sidebar_width = value
                .as_i64()
                .ok_or("sidebar_width must be integer")? as i32;
        }

        "appearance.topbar_height" => {
            settings.appearance.topbar_height = value
                .as_i64()
                .ok_or("topbar_height must be integer")? as i32;
        }

        "appearance.icon_size" => {
            settings.appearance.icon_size = value
                .as_i64()
                .ok_or("icon_size must be integer")? as i32;
        }

        "appearance.animations" => {
            settings.appearance.animations = value
                .as_bool()
                .ok_or("animations must be bool")?;
        }

        "input.pointer_speed" => {
            settings.input.pointer_speed = value
                .as_f64()
                .ok_or("pointer_speed must be number")? as f32;
        }

        "input.natural_scroll" => {
            settings.input.natural_scroll = value
                .as_bool()
                .ok_or("natural_scroll must be bool")?;
        }

        "apps.terminal" => {
            settings.apps.terminal = value
                .as_str()
                .ok_or("terminal must be string")?
                .to_string();
        }

        "apps.browser" => {
            settings.apps.browser = value
                .as_str()
                .ok_or("browser must be string")?
                .to_string();
        }

        "apps.file_manager" => {
            settings.apps.file_manager = value
                .as_str()
                .ok_or("file_manager must be string")?
                .to_string();
        }

        _ => return Err(format!("unknown setting path: {path}")),
    }

    Ok(())
}
