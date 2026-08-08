use anyhow::{Context, bail};
use focaldesk_ai::providers::OllamaProvider;
use focaldesk_ai::{AiIpcRequest, AiIpcResponse, AiProvider, ChatRequest, send_ai_request};
use focaldesk_diagnostics::{DiagnosticsOptions, collect_diagnostics};
use focaldesk_ipc::{
    IpcRequest, IpcResponse, NotificationIpcRequest, NotificationIpcResponse, send_desktop_request,
    send_notification_request,
};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };

    match command.as_str() {
        "notify" => {
            let title = args.next().context("notify requires a title")?;
            let mut timeout_ms = None;
            let mut body_parts = Vec::new();

            while let Some(arg) = args.next() {
                if arg == "--timeout-ms" {
                    let value = args.next().context("--timeout-ms requires a value")?;
                    timeout_ms = Some(value.parse::<u64>().context("invalid timeout value")?);
                } else {
                    body_parts.push(arg);
                }
            }

            let response = send_notification_request(&NotificationIpcRequest::Notify {
                title,
                body: body_parts.join(" "),
                timeout_ms,
            })
            .map_err(anyhow::Error::msg)?;

            match response {
                NotificationIpcResponse::NotificationQueued { id } => {
                    println!("notification queued: {id}");
                    Ok(())
                }
                NotificationIpcResponse::Ok => Ok(()),
                NotificationIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "identify-displays" => {
            let response =
                send_desktop_request(&IpcRequest::IdentifyDisplays).map_err(anyhow::Error::msg)?;
            match response {
                IpcResponse::Ok => Ok(()),
                IpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "diagnostics" => handle_diagnostics(args.collect()),
        "ai" => handle_ai(args.collect()),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
}

fn handle_diagnostics(args: Vec<String>) -> anyhow::Result<()> {
    let options = parse_diagnostics_options(args)?;
    let report = collect_diagnostics(&options).with_context(|| {
        format!(
            "could not create diagnostics archive at {}",
            options.output.display()
        )
    })?;
    println!("{}", report.path.display());
    eprintln!(
        "collected {} artifacts ({} uncompressed bytes); review before sharing",
        report.artifact_count, report.uncompressed_bytes
    );
    Ok(())
}

fn parse_diagnostics_options(args: Vec<String>) -> anyhow::Result<DiagnosticsOptions> {
    let mut options = DiagnosticsOptions::default();
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--output" => {
                options.output = PathBuf::from(args.next().context("--output requires a path")?);
            }
            "--no-logs" => options.include_logs = false,
            other => bail!("unknown diagnostics option: {other}"),
        }
    }
    Ok(options)
}

fn handle_ai(args: Vec<String>) -> anyhow::Result<()> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };

    match command.as_str() {
        "providers" => {
            let response = send_ai_request(&AiIpcRequest::ListProviders)?;
            match response {
                AiIpcResponse::Providers {
                    default_provider,
                    providers,
                } => {
                    println!("default: {default_provider}");
                    for provider in providers {
                        let model = provider.default_model.as_deref().unwrap_or("-");
                        let base_url = provider.base_url.as_deref().unwrap_or("-");
                        println!(
                            "{}\tkind={}\tmodel={}\tbase_url={}",
                            provider.id, provider.kind, model, base_url
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "chat" => {
            let mut provider = None;
            let mut model = None;
            let mut allow_fallback = !env_flag_enabled("FOCALDESK_CLI_NO_FALLBACK");
            let mut prompt_parts = Vec::new();

            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--provider" => {
                        provider = Some(args.next().context("--provider requires a value")?);
                    }
                    "--model" => {
                        model = Some(args.next().context("--model requires a value")?);
                    }
                    "--no-fallback" => {
                        allow_fallback = false;
                    }
                    _ => prompt_parts.push(arg),
                }
            }

            if prompt_parts.is_empty() {
                bail!("ai chat requires a prompt");
            }

            let mut request = ChatRequest::from_prompt(prompt_parts.join(" "));
            request.provider = provider;
            request.model = model;

            let execution = chat_via_ipc_or_ollama(request, allow_fallback)?;
            eprintln!(
                "[ai] path={} provider={} model={}",
                execution.path,
                execution.provider,
                execution.model.as_deref().unwrap_or("-")
            );

            let output = render_ai_output(&execution.content);
            if !output.is_empty() {
                print!("{output}");
                if !output.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        other => bail!("unknown ai command: {other}"),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  focaldesk-cli notify <title> [body...] [--timeout-ms <ms>]");
    eprintln!("  focaldesk-cli identify-displays");
    eprintln!("  focaldesk-cli diagnostics [--output <archive.tar.gz>] [--no-logs]");
    eprintln!("  focaldesk-cli ai providers");
    eprintln!(
        "  focaldesk-cli ai chat [--provider <id>] [--model <model>] [--no-fallback] <prompt...>"
    );
}

fn render_ai_output(content: &str) -> String {
    let normalized = strip_terminal_sequences(content)
        .chars()
        .map(normalize_line_separator)
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let normalized = normalized.trim();
    let mut output = String::with_capacity(normalized.len());
    let mut line_count = 0usize;
    let mut pending_blank_line = false;
    const MAX_LINES: usize = 128;
    const MAX_CHARS: usize = 8 * 1024;

    for line in normalized.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            pending_blank_line = line_count > 0;
            continue;
        }
        if pending_blank_line {
            if line_count >= MAX_LINES || output.len() >= MAX_CHARS {
                output.push_str("\n[output truncated]\n");
                return output;
            }
            output.push('\n');
            line_count += 1;
            pending_blank_line = false;
        }
        if line_count >= MAX_LINES || output.len() >= MAX_CHARS {
            output.push_str("\n[output truncated]\n");
            return output;
        }
        output.push_str(line);
        output.push('\n');
        line_count += 1;
    }

    output
}

fn normalize_line_separator(ch: char) -> char {
    match ch {
        '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{0b}' | '\u{0c}' => '\n',
        other => other,
    }
}

struct AiExecution {
    path: &'static str,
    provider: String,
    model: Option<String>,
    content: String,
}

fn chat_via_ipc_or_ollama(
    request: ChatRequest,
    allow_fallback: bool,
) -> anyhow::Result<AiExecution> {
    let fallback_to_ollama =
        allow_fallback && request.provider.as_deref().unwrap_or("ollama") == "ollama";

    match send_ai_request(&AiIpcRequest::Chat {
        request: request.clone(),
    }) {
        Ok(AiIpcResponse::Chat { response }) => Ok(AiExecution {
            path: "ipc",
            provider: response.provider,
            model: response.model,
            content: response.content,
        }),
        Ok(AiIpcResponse::Error { message })
            if fallback_to_ollama && is_ollama_daemon_error(&message) =>
        {
            eprintln!("[ai] ipc returned ollama error; falling back to direct ollama");
            direct_ollama_chat(request)
        }
        Ok(AiIpcResponse::Error { message }) => bail!(message),
        Ok(other) => bail!("unexpected AI response: {other:?}"),
        Err(err) if fallback_to_ollama && is_ipc_connection_error(&err) => {
            eprintln!("[ai] ipc unavailable; falling back to direct ollama");
            direct_ollama_chat(request)
        }
        Err(err) => Err(err),
    }
}

fn env_flag_enabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn is_ipc_connection_error(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    message.contains("could not connect to AI IPC socket")
        || message.contains("Operation not permitted")
        || message.contains("Connection refused")
        || message.contains("No such file or directory")
}

fn is_ollama_daemon_error(message: &str) -> bool {
    message.contains("no model configured for Ollama provider")
        || message.contains("Ollama returned HTTP")
        || message.contains("failed to parse Ollama chat response")
        || message.contains("Ollama chat request failed")
}

fn direct_ollama_chat(request: ChatRequest) -> anyhow::Result<AiExecution> {
    let base_url = std::env::var("FOCALDESK_OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let requested_model = request.model.clone();
    let provider = OllamaProvider::new(base_url, None)?;
    let runtime = tokio::runtime::Runtime::new().context("failed to create async runtime")?;

    let response = runtime.block_on(async move { provider.chat(request).await })?;
    let chosen_model = response
        .model
        .as_deref()
        .or(requested_model.as_deref())
        .unwrap_or("<unknown>");

    Ok(AiExecution {
        path: "direct-ollama",
        provider: "ollama".into(),
        model: Some(chosen_model.to_string()),
        content: response.content,
    })
}

fn strip_terminal_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    let _ = chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let _ = chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && matches!(chars.peek(), Some('\\')) {
                            let _ = chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    let _ = chars.next();
                }
                None => {}
            },
            '\n' | '\t' => output.push(ch),
            ch if ch.is_control() => {}
            ch => output.push(ch),
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{parse_diagnostics_options, render_ai_output};
    use std::path::Path;

    #[test]
    fn diagnostics_options_support_private_log_free_bundles() {
        let options = parse_diagnostics_options(vec![
            "--no-logs".into(),
            "--output".into(),
            "report.tar.gz".into(),
        ])
        .unwrap();
        assert!(!options.include_logs);
        assert_eq!(options.output, Path::new("report.tar.gz"));
    }

    #[test]
    fn normalize_ai_output_converts_crlf() {
        assert_eq!(render_ai_output("hello\r\nworld\r\n"), "hello\nworld\n");
    }

    #[test]
    fn normalize_ai_output_drops_trailing_blank_lines() {
        assert_eq!(render_ai_output("hello\n\nworld\n\n"), "hello\n\nworld\n");
    }

    #[test]
    fn normalize_ai_output_leaves_internal_newlines_intact() {
        assert_eq!(render_ai_output("hello\r\n\nworld"), "hello\n\nworld\n");
    }

    #[test]
    fn normalize_ai_output_collapses_blank_line_runs() {
        assert_eq!(render_ai_output("hello\n\n\n\nworld"), "hello\n\nworld\n");
    }

    #[test]
    fn normalize_ai_output_removes_whitespace_only_lines() {
        assert_eq!(
            render_ai_output("hello\n   \n\t\nworld"),
            "hello\n\nworld\n"
        );
    }

    #[test]
    fn normalize_ai_output_strips_ansi_sequences() {
        assert_eq!(
            render_ai_output("\u{1b}[31mhello\u{1b}[0m\nworld"),
            "hello\nworld\n"
        );
    }

    #[test]
    fn normalize_ai_output_suppresses_empty_output() {
        assert_eq!(render_ai_output("\n \n\t\n"), "");
    }

    #[test]
    fn normalize_ai_output_trims_outer_whitespace() {
        assert_eq!(render_ai_output("\n\n  hello world  \n\n"), "hello world\n");
    }

    #[test]
    fn normalize_ai_output_collapses_blank_lines_to_one() {
        assert_eq!(
            render_ai_output("hello\n\n\n\nworld\n\nthere"),
            "hello\n\nworld\n\nthere\n"
        );
    }

    #[test]
    fn normalize_ai_output_converts_unicode_line_separators() {
        assert_eq!(
            render_ai_output("\u{2028}\u{2028}hello\u{2029}\u{2028}world\u{2028}\u{2028}"),
            "hello\n\nworld\n"
        );
    }
}
