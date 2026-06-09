use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const DEFAULT_OUTPUT: &str = "flowstate-nested";
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
    let stdin_choices = read_stdin_choices();
    if !stdin_choices.is_empty() {
        return stdin_choices;
    }

    if let Ok(output) = env::var("FLOWSTATE_SCREENCAST_OUTPUT") {
        let output = output.trim();
        if !output.is_empty() {
            return vec![monitor_choice(output)];
        }
    }

    let wayland_outputs = configured_outputs().unwrap_or_else(query_wayland_outputs);
    if !wayland_outputs.is_empty() {
        return wayland_outputs
            .into_iter()
            .map(|output| monitor_choice(&output))
            .collect();
    }

    vec![monitor_choice(DEFAULT_OUTPUT)]
}

fn read_stdin_choices() -> Vec<String> {
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
        .map(ToOwned::to_owned)
        .collect()
}

fn query_wayland_outputs() -> Vec<String> {
    let output = match Command::new("wayland-info").output() {
        Ok(output) if output.status.success() => output.stdout,
        _ => return Vec::new(),
    };

    let Ok(text) = String::from_utf8(output) else {
        return Vec::new();
    };

    parse_wayland_output_names(&text)
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

    base.join("flowstate").join("displays.json")
}

fn parse_wayland_output_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("name: ")
                .map(str::trim)
                .map(trim_quotes)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn trim_quotes(value: &str) -> &str {
    value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(value)
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

    if let Some(command) = env::var("FLOWSTATE_SCREENCAST_CHOOSER")
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
    let exe = env::current_exe().context("failed to resolve flowstate-portal executable path")?;
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
    println!("Flowstate xdg-desktop-portal-wlr screencast chooser");
    println!();
    println!("Usage:");
    println!("  flowstate-portal");
    println!("  flowstate-portal --print-xdpw-config");
    println!();
    println!("Environment:");
    println!(
        "  FLOWSTATE_SCREENCAST_OUTPUT   Output name to use when no chooser input is provided"
    );
    println!("  FLOWSTATE_SCREENCAST_CHOOSER  Menu command to use before built-in menu discovery");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_wayland_info_output_names() {
        let text = r#"
interface: 'wl_output', version: 4, name: 42
	name: 'HDMI-A-1'
	description: 'FlowState display'
interface: 'wl_output', version: 4, name: 43
	name: 'DP-1'
"#;

        assert_eq!(
            parse_wayland_output_names(text),
            vec!["HDMI-A-1".to_string(), "DP-1".to_string()]
        );
    }

    #[test]
    fn formats_monitor_choice_for_xdpw_simple_chooser() {
        assert_eq!(monitor_choice("HDMI-A-1"), "Monitor: HDMI-A-1");
    }

    #[test]
    fn shell_quotes_paths_for_xdpw_config() {
        assert_eq!(
            shell_quote("/usr/bin/flowstate-portal"),
            "/usr/bin/flowstate-portal"
        );
        assert_eq!(
            shell_quote("/tmp/flow state/portal's/bin"),
            "'/tmp/flow state/portal'\"'\"'s/bin'"
        );
    }
}
