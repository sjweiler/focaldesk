use anyhow::{Context, bail};
use focaldesk_ai::{
    AgentRequest, AiIpcRequest, AiIpcResponse, AiStreamEvent, ChatRequest, send_ai_request,
    stream_ai_chat,
};
use focaldesk_diagnostics::{DiagnosticsOptions, collect_diagnostics};
use focaldesk_ipc::{
    IpcRequest, IpcResponse, NotificationIpcRequest, NotificationIpcResponse, send_desktop_request,
    send_notification_request,
};
use std::io::{self, Write};
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
            let mut stream = false;
            let mut prompt_parts = Vec::new();

            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--provider" => {
                        provider = Some(args.next().context("--provider requires a value")?);
                    }
                    "--model" => {
                        model = Some(args.next().context("--model requires a value")?);
                    }
                    "--stream" => stream = true,
                    _ => prompt_parts.push(arg),
                }
            }

            if prompt_parts.is_empty() {
                bail!("ai chat requires a prompt");
            }

            let mut request = ChatRequest::from_prompt(prompt_parts.join(" "));
            request.provider = provider;
            request.model = model;

            if stream {
                return chat_stream_via_ipc(request);
            }

            let execution = chat_via_ipc(request)?;
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
        "agent" => {
            let mut provider = None;
            let mut model = None;
            let mut objective_parts = Vec::new();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--provider" => {
                        provider = Some(args.next().context("--provider requires a value")?);
                    }
                    "--model" => {
                        model = Some(args.next().context("--model requires a value")?);
                    }
                    _ => objective_parts.push(arg),
                }
            }
            if objective_parts.is_empty() {
                bail!("ai agent requires an objective");
            }
            match send_ai_request(&AiIpcRequest::RunAgent {
                request: AgentRequest {
                    objective: objective_parts.join(" "),
                    provider,
                    model,
                },
            })? {
                AiIpcResponse::Agent { response } => {
                    eprintln!(
                        "[ai-agent] provider={} model={} tools={}",
                        response.provider,
                        response.model.as_deref().unwrap_or("-"),
                        response.steps.len()
                    );
                    let output = render_ai_output(&response.answer);
                    if !output.is_empty() {
                        print!("{output}");
                    }
                    if let Some(confirmation) = response.confirmation {
                        eprintln!(
                            "[ai-agent] pending action: {} {}",
                            confirmation.tool, confirmation.arguments
                        );
                        eprintln!(
                            "[ai-agent] approve before {} with: focaldesk-cli ai confirm {}",
                            confirmation.expires_at_unix, confirmation.plan_id
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "confirm" | "deny" => {
            let approved = command == "confirm";
            let plan_id = args
                .next()
                .with_context(|| format!("ai {command} requires a plan id"))?;
            if args.next().is_some() {
                bail!("ai {command} accepts exactly one plan id");
            }
            match send_ai_request(&AiIpcRequest::ConfirmAgentAction { plan_id, approved })? {
                AiIpcResponse::AgentAction { response } => {
                    if response.executed {
                        println!("executed {}", response.tool);
                    } else {
                        println!("denied {}", response.tool);
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
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
    eprintln!("  focaldesk-cli ai chat [--stream] [--provider <id>] [--model <model>] <prompt...>");
    eprintln!("  focaldesk-cli ai agent [--provider <id>] [--model <model>] <objective...>");
    eprintln!("  focaldesk-cli ai confirm <plan-id>");
    eprintln!("  focaldesk-cli ai deny <plan-id>");
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

fn chat_via_ipc(request: ChatRequest) -> anyhow::Result<AiExecution> {
    match send_ai_request(&AiIpcRequest::Chat { request }) {
        Ok(AiIpcResponse::Chat { response }) => Ok(AiExecution {
            path: "ipc",
            provider: response.provider,
            model: response.model,
            content: response.content,
        }),
        Ok(AiIpcResponse::Error { message }) => bail!(message),
        Ok(other) => bail!("unexpected AI response: {other:?}"),
        Err(err) => Err(err).context(
            "AI chat requires focaldesk-server so requests remain permission-gated and audited",
        ),
    }
}

fn chat_stream_via_ipc(request: ChatRequest) -> anyhow::Result<()> {
    let mut printed = false;
    let mut ends_with_newline = false;
    let result = stream_ai_chat(request, |event| {
        match event {
            AiStreamEvent::Started {
                provider, model, ..
            } => {
                eprintln!(
                    "[ai] path=ipc-stream provider={} model={}",
                    provider,
                    model.as_deref().unwrap_or("-")
                );
            }
            AiStreamEvent::Delta { content, .. } => {
                let output = strip_terminal_sequences(&content);
                if !output.is_empty() {
                    print!("{output}");
                    io::stdout().flush().context("flush streamed AI output")?;
                    printed = true;
                    ends_with_newline = output.ends_with('\n');
                }
            }
            AiStreamEvent::Completed { .. } => {
                if printed && !ends_with_newline {
                    println!();
                }
            }
            AiStreamEvent::Failed { message, .. } => bail!(message),
            AiStreamEvent::Cancelled { .. } => bail!("AI stream was cancelled"),
        }
        Ok(())
    });
    result
        .map(|_| ())
        .context("streaming AI chat requires a protocol-v2 focaldesk-server")
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
