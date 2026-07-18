use focaldesk_ipc::{
    ControlIpcRequest, ControlIpcResponse, ControlSetting, NotificationIpcRequest,
    NotificationIpcResponse, send_control_request, send_notification_request,
};
use mlua::{Lua, MultiValue, Value};
use std::path::Path;

/// Runs `path` in a fresh Lua VM (no state persists between runs) with the
/// structured API installed. Blocking — callers on an async runtime should
/// wrap this in `spawn_blocking`.
pub fn run_script(automation_name: &str, path: &Path) -> Result<(), String> {
    let source = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;

    let lua = Lua::new();
    install_api(&lua, automation_name).map_err(|err| err.to_string())?;

    lua.load(&source)
        .set_name(automation_name)
        .exec()
        .map_err(|err| format!("lua error in '{automation_name}': {err}"))
}

fn install_api(lua: &Lua, automation_name: &str) -> mlua::Result<()> {
    let globals = lua.globals();

    let name_for_log = automation_name.to_string();
    globals.set(
        "log",
        lua.create_function(move |_, args: MultiValue| {
            let message = args
                .iter()
                .map(describe_lua_value)
                .collect::<Vec<_>>()
                .join(" ");
            tracing::info!(
                target: "focaldesk.automation",
                automation = %name_for_log,
                "{message}"
            );
            Ok(())
        })?,
    )?;

    globals.set(
        "exec",
        lua.create_function(|_, (cmd, args): (String, Option<Vec<String>>)| {
            let output = std::process::Command::new(&cmd)
                .args(args.unwrap_or_default())
                .output()
                .map_err(|err| mlua::Error::RuntimeError(format!("{cmd}: {err}")))?;

            Ok((
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        })?,
    )?;

    globals.set(
        "notify",
        lua.create_function(
            |_, (title, body): (String, String)| match send_notification_request(
                &NotificationIpcRequest::Notify {
                    title,
                    body,
                    timeout_ms: None,
                },
            ) {
                Ok(NotificationIpcResponse::Error { message }) => {
                    Err(mlua::Error::RuntimeError(message))
                }
                Ok(_) => Ok(()),
                Err(err) => Err(mlua::Error::RuntimeError(err)),
            },
        )?,
    )?;

    globals.set(
        "volume",
        lua.create_function(|_, level: f32| {
            control_request(ControlIpcRequest::SetVolume { volume: level })
        })?,
    )?;

    globals.set(
        "bluetooth_power",
        lua.create_function(|_, enabled: bool| {
            control_request(ControlIpcRequest::SetSystemSetting {
                setting: ControlSetting::Bluetooth,
                enabled,
            })
        })?,
    )?;

    globals.set(
        "wifi_power",
        lua.create_function(|_, enabled: bool| {
            control_request(ControlIpcRequest::SetSystemSetting {
                setting: ControlSetting::Wifi,
                enabled,
            })
        })?,
    )?;

    Ok(())
}

fn control_request(request: ControlIpcRequest) -> mlua::Result<()> {
    match send_control_request(&request) {
        Ok(ControlIpcResponse::Error { message }) => Err(mlua::Error::RuntimeError(message)),
        Ok(ControlIpcResponse::Ok) => Ok(()),
        Err(err) => Err(mlua::Error::RuntimeError(err)),
    }
}

fn describe_lua_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string_lossy(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempScript(PathBuf);

    impl TempScript {
        fn new(source: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "focaldesk-automation-test-{}-{nanos}.lua",
                std::process::id()
            ));
            std::fs::write(&path, source).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempScript {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn runs_a_trivial_script() {
        let script = TempScript::new("log('hello from test')");
        run_script("trivial", script.path()).expect("script should succeed");
    }

    #[test]
    fn exec_returns_exit_code_and_output() {
        let script = TempScript::new(
            r#"
                local code, stdout, _ = exec("sh", {"-c", "echo hi"})
                assert(code == 0, "expected exit code 0")
                assert(stdout == "hi\n", "unexpected stdout: " .. stdout)
            "#,
        );
        run_script("exec-check", script.path()).expect("script should succeed");
    }

    #[test]
    fn lua_errors_surface_as_err() {
        let script = TempScript::new("error('boom')");
        let result = run_script("failing", script.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("boom"));
    }

    #[test]
    fn missing_file_is_a_readable_error() {
        let result = run_script("missing", Path::new("/nonexistent/path.lua"));
        assert!(result.is_err());
    }
}
