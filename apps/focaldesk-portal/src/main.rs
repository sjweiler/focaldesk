use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

mod wayland_outputs;

const DEFAULT_OUTPUT: &str = "focaldesk-nested";
const MENU_COMMANDS: &[MenuCommand<'_>] = &[
    MenuCommand {
        program: "zenity",
        args: &[
            "--list",
            "--width=520",
            "--height=320",
            "--title=Select a source to share",
            "--text=Select a source to share",
            "--column=Source",
        ],
    },
    MenuCommand {
        program: "fuzzel",
        args: &["--dmenu"],
    },
    MenuCommand {
        program: "wofi",
        args: &["--dmenu"],
    },
    MenuCommand {
        program: "wmenu",
        args: &[],
    },
    MenuCommand {
        program: "bemenu",
        args: &[],
    },
    MenuCommand {
        program: "rofi",
        args: &["-dmenu"],
    },
    MenuCommand {
        program: "dmenu",
        args: &[],
    },
];

#[derive(Debug, Clone, Copy)]
struct MenuCommand<'a> {
    program: &'a str,
    args: &'a [&'a str],
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }

    if args.iter().any(|arg| arg == "--print-xdpw-config") {
        return print_xdpw_config();
    }

    run_chooser()
}

fn run_chooser() -> Result<()> {
    let choices = screencast_choices();
    if choices.is_empty() {
        return Ok(());
    }

    let selected = select_choice(&choices)?;
    if let Some(selected) = selected {
        println!("{}", selected);
    }

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

    vec![monitor_choice(DEFAULT_OUTPUT)]
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
    if choices.len() == 1 {
        return Ok(Some(choices[0].clone()));
    }

    if let Some(command) = env::var("FOCALDESK_SCREENCAST_CHOOSER")
        .ok()
        .filter(|command| !command.trim().is_empty())
    {
        return run_menu_command(
            MenuCommand {
                program: command.as_str(),
                args: &[],
            },
            choices,
        );
    }

    for menu in MENU_COMMANDS {
        if command_exists(menu.program) {
            match run_menu_command(*menu, choices)? {
                Some(selected) => return Ok(Some(selected)),
                None => continue,
            }
        }
    }

    Ok(Some(choices[0].clone()))
}

fn command_exists(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|path| path.join(command).is_file())
}

fn run_menu_command(command: MenuCommand<'_>, choices: &[String]) -> Result<Option<String>> {
    let mut child = Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start chooser command `{}`", command.program))?;

    if let Some(mut stdin) = child.stdin.take() {
        for choice in choices {
            writeln!(stdin, "{choice}")?;
        }
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to read chooser command `{}`", command.program))?;

    if !output.status.success() {
        return Ok(None);
    }

    let selected = String::from_utf8_lossy(&output.stdout);
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(None);
    }

    if choices.iter().any(|choice| choice == selected) {
        return Ok(Some(selected.to_owned()));
    }

    if selected.starts_with("Monitor: ") || selected.starts_with("Window: ") {
        return Ok(Some(selected.to_owned()));
    }

    Ok(Some(monitor_choice(selected)))
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
    println!("  focaldesk-portal --print-xdpw-config");
    println!();
    println!("Environment:");
    println!(
        "  FOCALDESK_SCREENCAST_OUTPUT   Output name to use when no chooser input is provided"
    );
    println!("  FOCALDESK_SCREENCAST_CHOOSER  Menu command to use before built-in menu discovery");
}

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
    fn formats_monitor_choice_for_xdpw_simple_chooser() {
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
