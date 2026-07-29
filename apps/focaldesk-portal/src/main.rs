use anyhow::{Context, Result};
use focaldesk_ipc::{
    DialogIpcRequest, DialogIpcResponse, IpcRequest, IpcResponse, send_desktop_request,
    send_dialog_request,
};
use serde::Deserialize;
use std::env;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

mod backend;
mod wayland_outputs;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }

    if args.iter().any(|arg| arg == "--print-xdpw-config") {
        return print_xdpw_config();
    }

    if args.iter().any(|arg| arg == "--backend") {
        return backend::run().await;
    }

    run_chooser()
}

fn run_chooser() -> Result<()> {
    let choices = screencast_choices();
    if choices.is_empty() {
        return Err(anyhow::anyhow!(
            "could not discover any screencast sources from stdin, config, or Wayland"
        ));
    }

    let selected = select_choice(&choices)?;
    let Some(selected) = selected else {
        return Err(anyhow::anyhow!("no screencast source was selected"));
    };

    println!("{}", selected);
    Ok(())
}

fn screencast_choices() -> Vec<String> {
    // xdpw `dmenu` chooser pipes "Monitor: …" lines on stdin; `simple` sends nothing.
    let dmenu_choices = read_dmenu_choices_from_stdin();
    if !dmenu_choices.is_empty() {
        return dmenu_choices;
    }

    if let Ok(output) = env::var("FOCALDESK_SCREENCAST_OUTPUT") {
        let output = output.trim();
        if !output.is_empty() {
            return vec![monitor_choice(output)];
        }
    }

    let mut outputs = wayland_outputs::query_wayland_outputs();
    if let Some(configured) = configured_outputs() {
        for name in configured {
            if !outputs.iter().any(|existing| existing == &name) {
                outputs.push(name);
            }
        }
    }
    if !outputs.is_empty() {
        return outputs
            .into_iter()
            .map(|output| monitor_choice(&output))
            .collect();
    }

    if let Some(outputs) = desktop_runtime_outputs() {
        return outputs
            .into_iter()
            .map(|output| monitor_choice(&output))
            .collect();
    }

    Vec::new()
}

/// Read xdpw `dmenu`-style chooser input. Ignore empty stdin (`simple` chooser type).
fn read_dmenu_choices_from_stdin() -> Vec<String> {
    let mut stdin = io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }

    let mut input = String::new();
    if stdin.read_to_string(&mut input).is_err() {
        return Vec::new();
    }

    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| line.starts_with("Monitor: ") || line.starts_with("Window: "))
        .map(ToOwned::to_owned)
        .collect()
}

fn configured_outputs() -> Option<Vec<String>> {
    let path = displays_config_path();
    let text = std::fs::read_to_string(path).ok()?;
    let displays: Vec<DisplayConfig> = serde_json::from_str(&text).ok()?;
    let outputs = displays
        .into_iter()
        .filter(|display| display.enabled)
        .map(|display| display.name)
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        None
    } else {
        Some(outputs)
    }
}

fn displays_config_path() -> PathBuf {
    let base = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });

    base.join("focaldesk").join("displays.json")
}

#[derive(Debug, Clone, Deserialize)]
struct DisplayConfig {
    name: String,
    enabled: bool,
}

fn monitor_choice(output: &str) -> String {
    format!("Monitor: {output}")
}

fn select_choice(choices: &[String]) -> Result<Option<String>> {
    prompt_choice_from_desktop(choices)
}

fn desktop_runtime_outputs() -> Option<Vec<String>> {
    let response = send_desktop_request(&IpcRequest::GetDisplayRuntimeStatus).ok()?;

    match response {
        IpcResponse::DisplayRuntimeStatus { outputs } => {
            let outputs = outputs
                .into_iter()
                .map(|output| output.connector)
                .filter(|connector| !connector.trim().is_empty())
                .collect::<Vec<_>>();
            if outputs.is_empty() {
                None
            } else {
                Some(outputs)
            }
        }
        _ => None,
    }
}

fn prompt_choice_from_desktop(choices: &[String]) -> Result<Option<String>> {
    let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
    let response = send_dialog_request(&DialogIpcRequest::PortalChooserPrompt {
        request_id,
        title: "Select a source to share".to_string(),
        message: "Choose the monitor or window that OBS should capture.".to_string(),
        choices: choices.to_vec(),
    });

    match response {
        Ok(DialogIpcResponse::PortalChooserDecision {
            request_id: response_id,
            selected,
        }) if response_id == request_id => Ok(selected),
        Ok(DialogIpcResponse::Error { message }) => Err(anyhow::anyhow!(message)),
        Ok(other) => Err(anyhow::anyhow!(
            "unexpected IPC response from desktop chooser: {other:?}"
        )),
        Err(err) => Err(anyhow::anyhow!(err)),
    }
}

fn print_xdpw_config() -> Result<()> {
    let exe = env::current_exe().context("failed to resolve focaldesk-portal executable path")?;
    let exe = exe.to_string_lossy();

    println!("[screencast]");
    println!("chooser_type=simple");
    println!("chooser_cmd={}", shell_quote(&exe));

    Ok(())
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn print_help() {
    println!("FocalDesk xdg-desktop-portal-wlr screencast chooser");
    println!();
    println!("Usage:");
    println!("  focaldesk-portal");
    println!("  focaldesk-portal --backend");
    println!("  focaldesk-portal --print-xdpw-config");
    println!();
    println!("Environment:");
    println!(
        "  FOCALDESK_SCREENCAST_OUTPUT   Output name to use when no chooser input is provided"
    );
}

static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_enabled_outputs_from_displays_json() {
        let json = r#"[
          {"name":"DP-3","enabled":true},
          {"name":"DP-4","enabled":false}
        ]"#;
        let displays: Vec<DisplayConfig> = serde_json::from_str(json).unwrap();
        let outputs = displays
            .into_iter()
            .filter(|display| display.enabled)
            .map(|display| display.name)
            .collect::<Vec<_>>();
        assert_eq!(outputs, vec!["DP-3".to_string()]);
    }

    #[test]
    fn formats_monitor_choice_for_xdpw_dmenu_chooser() {
        assert_eq!(monitor_choice("HDMI-A-1"), "Monitor: HDMI-A-1");
    }

    #[test]
    fn shell_quotes_paths_for_xdpw_config() {
        assert_eq!(
            shell_quote("/usr/bin/focaldesk-portal"),
            "/usr/bin/focaldesk-portal"
        );
        assert_eq!(
            shell_quote("/tmp/flow state/portal's/bin"),
            "'/tmp/flow state/portal'\"'\"'s/bin'"
        );
    }
}
