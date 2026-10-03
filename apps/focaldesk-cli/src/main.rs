use anyhow::{Context, bail};
use focaldesk_ai::{
    AgentRequest, AiIpcRequest, AiIpcResponse, AiStreamEvent, ChatRequest, Citation,
    RetrievalEvalCase, send_ai_request, stream_ai_chat,
};
use focaldesk_diagnostics::{DiagnosticsOptions, collect_diagnostics};
use focaldesk_ipc::{
    DesktopAction, DesktopDirection, DesktopSnapshot, DesktopSplitKeyboardAction,
    DesktopSplitLayout, IpcRequest, IpcResponse, NotificationIpcRequest, NotificationIpcResponse,
    send_desktop_request, send_notification_request,
};
use focaldesk_settings_core::{DisplayColorProfile, HdrAppearance, OutputConfig};
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
        "desktop-snapshot" => {
            let response = send_desktop_request(&IpcRequest::GetDesktopSnapshot)
                .map_err(anyhow::Error::msg)?;
            match response {
                IpcResponse::DesktopSnapshot { snapshot } => {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                    Ok(())
                }
                IpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "window-geometry" => {
            let title = args.next().context("window-geometry requires a title")?;
            let snapshot = desktop_snapshot()?;
            let window = snapshot
                .windows
                .iter()
                .find(|window| window.title == title)
                .with_context(|| format!("window `{title}` was not found"))?;
            println!(
                "{} {} {} {}",
                window.x.context("window has no x coordinate")?,
                window.y.context("window has no y coordinate")?,
                window.width.context("window has no width")?,
                window.height.context("window has no height")?
            );
            Ok(())
        }
        "window-workspace" => {
            let title = args.next().context("window-workspace requires a title")?;
            let snapshot = desktop_snapshot()?;
            let window = snapshot
                .windows
                .iter()
                .find(|window| window.title == title)
                .with_context(|| format!("window `{title}` was not found"))?;
            println!("{}", window.workspace_id);
            Ok(())
        }
        "focused-window-title" => {
            let snapshot = desktop_snapshot()?;
            println!(
                "{}",
                snapshot.shell.focused_window_title.as_deref().unwrap_or("")
            );
            Ok(())
        }
        "split-resize-percent" => {
            let snapshot = desktop_snapshot()?;
            let percent = snapshot
                .rendering
                .split_resize_percent
                .context("split resize HUD is not visible")?;
            println!("{percent}");
            Ok(())
        }
        "window-move-workspace" => {
            let title = args
                .next()
                .context("window-move-workspace requires a title")?;
            let workspace = args
                .next()
                .context("window-move-workspace requires a workspace number")?
                .parse::<u32>()
                .context("workspace must be an integer")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::MoveWindowToWorkspace {
                window_id,
                workspace,
            })
        }
        "split-window" => {
            let title = args.next().context("split-window requires a title")?;
            let direction = match args
                .next()
                .context("split-window requires left, right, top, or bottom")?
                .as_str()
            {
                "left" => DesktopDirection::Left,
                "right" => DesktopDirection::Right,
                "top" => DesktopDirection::Up,
                "bottom" => DesktopDirection::Down,
                other => bail!("unknown split direction `{other}`"),
            };
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::SplitWindow {
                window_id,
                direction,
            })
        }
        "split-ratio" => {
            let title = args.next().context("split-ratio requires a title")?;
            let ratio_per_mille = args
                .next()
                .context("split-ratio requires a per-mille value")?
                .parse::<u16>()
                .context("split ratio must be an integer")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::SetSplitRatio {
                window_id,
                ratio_per_mille,
            })
        }
        "split-divider" => {
            let title = args.next().context("split-divider requires a title")?;
            let direction = parse_desktop_direction(
                &args.next().context("split-divider requires a direction")?,
            )?;
            let ratio_per_mille = args
                .next()
                .context("split-divider requires a per-mille value")?
                .parse::<u16>()
                .context("split divider ratio must be an integer")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::SetSplitDivider {
                window_id,
                direction,
                ratio_per_mille,
            })
        }
        "split-layout" => {
            let title = args.next().context("split-layout requires a title")?;
            let layout =
                parse_split_layout(&args.next().context("split-layout requires a layout")?)?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::ApplySplitLayout { window_id, layout })
        }
        "split-assist" => {
            let title = args.next().context("split-assist requires a title")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::SelectSplitAssistWindow { window_id })
        }
        "split-swap" => {
            let title = args.next().context("split-swap requires a title")?;
            let direction =
                parse_desktop_direction(&args.next().context("split-swap requires a direction")?)?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::SwapSplitWindow {
                window_id,
                direction,
            })
        }
        "split-key" => {
            let title = args.next().context("split-key requires a title")?;
            let command =
                parse_split_keyboard_action(&args.next().context("split-key requires a command")?)?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::InvokeSplitKeyboardAction { window_id, command })
        }
        "split-replace" => {
            let title = args.next().context("split-replace requires a title")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::ReplaceSplitWindow { window_id })
        }
        "split-exit" => {
            let title = args.next().context("split-exit requires a title")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::ExitSplitGroup { window_id })
        }
        "split-workspace" => {
            let title = args.next().context("split-workspace requires a title")?;
            let workspace = args
                .next()
                .context("split-workspace requires a workspace number")?
                .parse::<u32>()
                .context("workspace must be an integer")?;
            let window_id = window_id_for_title(&title)?;
            execute_desktop_action(DesktopAction::AssignSplitGroupToWorkspace {
                window_id,
                workspace,
            })
        }
        "reload-settings" => {
            match send_desktop_request(&IpcRequest::Reload).map_err(anyhow::Error::msg)? {
                IpcResponse::Ok => Ok(()),
                IpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "checkpoint-session" => execute_desktop_action(DesktopAction::CheckpointSession),
        "create-workspace" => execute_desktop_action(DesktopAction::CreateWorkspace),
        "focus-workspace" => {
            let workspace = args
                .next()
                .context("focus-workspace requires a workspace number")?
                .parse::<u32>()
                .context("workspace must be an integer")?;
            execute_desktop_action(DesktopAction::FocusWorkspace { workspace })
        }
        "display-mode" => {
            let connector = args.next().context("display-mode requires a connector")?;
            let width = args
                .next()
                .context("display-mode requires a width")?
                .parse::<i32>()
                .context("invalid display width")?;
            let height = args
                .next()
                .context("display-mode requires a height")?
                .parse::<i32>()
                .context("invalid display height")?;
            let scale = args
                .next()
                .context("display-mode requires a scale")?
                .parse::<f32>()
                .context("invalid display scale")?;
            let snapshot = desktop_snapshot()?;
            let output = snapshot
                .outputs
                .iter()
                .find(|output| output.connector == connector)
                .with_context(|| format!("display `{connector}` was not found"))?;
            match send_desktop_request(&IpcRequest::SetDisplays {
                outputs: vec![OutputConfig {
                    connector,
                    enabled: true,
                    x: output.x,
                    y: output.y,
                    width,
                    height,
                    refresh_mhz: output.refresh_mhz.max(60_000),
                    scale,
                    primary: true,
                    color_profile: DisplayColorProfile::Auto,
                    icc_profile_path: None,
                    hdr_requested: output.hdr_requested,
                    hdr_enabled: output.hdr_requested,
                    hdr_appearance: HdrAppearance::default(),
                }],
            })
            .map_err(anyhow::Error::msg)?
            {
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

fn desktop_snapshot() -> anyhow::Result<DesktopSnapshot> {
    match send_desktop_request(&IpcRequest::GetDesktopSnapshot).map_err(anyhow::Error::msg)? {
        IpcResponse::DesktopSnapshot { snapshot } => Ok(snapshot),
        IpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn window_id_for_title(title: &str) -> anyhow::Result<u32> {
    desktop_snapshot()?
        .windows
        .into_iter()
        .find(|window| window.title == title)
        .map(|window| window.id)
        .with_context(|| format!("window `{title}` was not found"))
}

fn parse_desktop_direction(value: &str) -> anyhow::Result<DesktopDirection> {
    match value {
        "left" => Ok(DesktopDirection::Left),
        "right" => Ok(DesktopDirection::Right),
        "top" | "up" => Ok(DesktopDirection::Up),
        "bottom" | "down" => Ok(DesktopDirection::Down),
        other => bail!("unknown direction `{other}`"),
    }
}

fn parse_split_layout(value: &str) -> anyhow::Result<DesktopSplitLayout> {
    match value {
        "left-half" => Ok(DesktopSplitLayout::LeftHalf),
        "right-half" => Ok(DesktopSplitLayout::RightHalf),
        "left-two-thirds" => Ok(DesktopSplitLayout::LeftTwoThirds),
        "right-third" => Ok(DesktopSplitLayout::RightThird),
        "left-third" => Ok(DesktopSplitLayout::LeftThird),
        "right-two-thirds" => Ok(DesktopSplitLayout::RightTwoThirds),
        "top-half" => Ok(DesktopSplitLayout::TopHalf),
        "bottom-half" => Ok(DesktopSplitLayout::BottomHalf),
        "top-left" => Ok(DesktopSplitLayout::TopLeft),
        "top-right" => Ok(DesktopSplitLayout::TopRight),
        "bottom-left" => Ok(DesktopSplitLayout::BottomLeft),
        "bottom-right" => Ok(DesktopSplitLayout::BottomRight),
        other => bail!("unknown split layout `{other}`"),
    }
}

fn parse_split_keyboard_action(value: &str) -> anyhow::Result<DesktopSplitKeyboardAction> {
    Ok(match value {
        "resize-left" => DesktopSplitKeyboardAction::ResizeLeft,
        "resize-right" => DesktopSplitKeyboardAction::ResizeRight,
        "resize-up" => DesktopSplitKeyboardAction::ResizeUp,
        "resize-down" => DesktopSplitKeyboardAction::ResizeDown,
        "resize-left-fine" => DesktopSplitKeyboardAction::ResizeLeftFine,
        "resize-right-fine" => DesktopSplitKeyboardAction::ResizeRightFine,
        "resize-up-fine" => DesktopSplitKeyboardAction::ResizeUpFine,
        "resize-down-fine" => DesktopSplitKeyboardAction::ResizeDownFine,
        "focus-next" => DesktopSplitKeyboardAction::FocusNext,
        "focus-previous" => DesktopSplitKeyboardAction::FocusPrevious,
        "undo" => DesktopSplitKeyboardAction::Undo,
        other => bail!("unknown split keyboard command `{other}`"),
    })
}

fn execute_desktop_action(action: DesktopAction) -> anyhow::Result<()> {
    match send_desktop_request(&IpcRequest::ExecuteDesktopAction { action })
        .map_err(anyhow::Error::msg)?
    {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => bail!(message),
        other => bail!("unexpected response: {other:?}"),
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
            let mut use_memory = false;
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
                    "--memory" => use_memory = true,
                    _ => prompt_parts.push(arg),
                }
            }

            if prompt_parts.is_empty() {
                bail!("ai chat requires a prompt");
            }

            let mut request = ChatRequest::from_prompt(prompt_parts.join(" "));
            request.provider = provider;
            request.model = model;
            request.use_memory = use_memory;

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
            print_citations(&execution.citations);
            Ok(())
        }
        "ingest" => {
            let mut path = None;
            let mut recursive = false;
            for argument in args {
                if argument == "--recursive" {
                    recursive = true;
                } else if argument.starts_with('-') {
                    bail!("unknown ai ingest option: {argument}");
                } else if path.replace(PathBuf::from(argument)).is_some() {
                    bail!("ai ingest accepts exactly one file or directory path");
                }
            }
            let path = path.context("ai ingest requires a file or directory path")?;
            if path.is_dir() {
                match send_ai_request(&AiIpcRequest::IngestDirectory { path, recursive })? {
                    AiIpcResponse::DirectoryIngested { result } => {
                        println!(
                            "indexed={} unchanged={} skipped={} failed={} chunks={} from {}",
                            result.indexed,
                            result.unchanged,
                            result.skipped,
                            result.failed,
                            result.chunks,
                            result.source
                        );
                        for error in result.errors {
                            eprintln!("warning: {error}");
                        }
                        Ok(())
                    }
                    AiIpcResponse::Error { message } => bail!(message),
                    other => bail!("unexpected AI response: {other:?}"),
                }
            } else {
                if recursive {
                    bail!("--recursive requires a directory path");
                }
                match send_ai_request(&AiIpcRequest::IngestDocument { path })? {
                    AiIpcResponse::DocumentIngested { result } => {
                        if result.unchanged {
                            println!(
                                "unchanged {} chunk(s) from {}",
                                result.chunks, result.source
                            );
                        } else {
                            println!("indexed {} chunk(s) from {}", result.chunks, result.source);
                        }
                        Ok(())
                    }
                    AiIpcResponse::Error { message } => bail!(message),
                    other => bail!("unexpected AI response: {other:?}"),
                }
            }
        }
        "sources" => match send_ai_request(&AiIpcRequest::ListIndexedDocuments)? {
            AiIpcResponse::IndexedDocuments { documents } => {
                for document in documents {
                    println!(
                        "{}\tchunks={}\ttype={}\tindexed={}",
                        document.source,
                        document.chunk_count,
                        document.media_type,
                        document.indexed_at_unix
                    );
                }
                Ok(())
            }
            AiIpcResponse::Error { message } => bail!(message),
            other => bail!("unexpected AI response: {other:?}"),
        },
        "remove-source" => {
            let source = args
                .next()
                .context("ai remove-source requires a canonical source path")?;
            if args.next().is_some() {
                bail!("ai remove-source accepts exactly one source path");
            }
            match send_ai_request(&AiIpcRequest::RemoveIndexedDocument {
                source: source.clone(),
            })? {
                AiIpcResponse::IndexedDocumentRemoved { removed: true, .. } => {
                    println!("removed {source}");
                    Ok(())
                }
                AiIpcResponse::IndexedDocumentRemoved { removed: false, .. } => {
                    bail!("indexed source not found: {source}")
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "eval" => {
            let path = args
                .next()
                .context("ai eval requires a JSON evaluation file")?;
            let mut top_k = 5usize;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--top-k" => {
                        top_k = args
                            .next()
                            .context("--top-k requires a value")?
                            .parse()
                            .context("invalid --top-k value")?;
                    }
                    _ => bail!("unknown ai eval option: {arg}"),
                }
            }
            let input = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read retrieval evaluation file {path}"))?;
            let cases: Vec<RetrievalEvalCase> = serde_json::from_str(&input)
                .with_context(|| format!("invalid retrieval evaluation JSON in {path}"))?;
            match send_ai_request(&AiIpcRequest::EvaluateRetrieval { cases, top_k })? {
                AiIpcResponse::RetrievalEvaluated { report } => {
                    println!(
                        "cases={} hits={} recall@{}={:.3} mrr={:.3}",
                        report.cases,
                        report.hits,
                        report.top_k,
                        report.recall_at_k,
                        report.mean_reciprocal_rank
                    );
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
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
    eprintln!("  focaldesk-cli desktop-snapshot");
    eprintln!("  focaldesk-cli window-geometry <exact-title>");
    eprintln!("  focaldesk-cli window-workspace <exact-title>");
    eprintln!("  focaldesk-cli focused-window-title");
    eprintln!("  focaldesk-cli window-move-workspace <exact-title> <workspace>");
    eprintln!("  focaldesk-cli split-window <exact-title> <left|right|top|bottom>");
    eprintln!("  focaldesk-cli split-ratio <exact-title> <per-mille>");
    eprintln!("  focaldesk-cli split-divider <exact-title> <direction> <per-mille>");
    eprintln!("  focaldesk-cli split-layout <exact-title> <layout>");
    eprintln!("  focaldesk-cli split-assist <exact-title>");
    eprintln!("  focaldesk-cli split-swap <exact-title> <left|right|top|bottom>");
    eprintln!("  focaldesk-cli split-key <exact-title> <command>");
    eprintln!("  focaldesk-cli split-resize-percent");
    eprintln!("  focaldesk-cli split-replace <exact-title>");
    eprintln!("  focaldesk-cli split-exit <exact-title>");
    eprintln!("  focaldesk-cli split-workspace <exact-title> <workspace>");
    eprintln!("  focaldesk-cli reload-settings");
    eprintln!("  focaldesk-cli checkpoint-session");
    eprintln!("  focaldesk-cli create-workspace");
    eprintln!("  focaldesk-cli focus-workspace <workspace>");
    eprintln!("  focaldesk-cli display-mode <connector> <width> <height> <scale>");
    eprintln!("  focaldesk-cli diagnostics [--output <archive.tar.gz>] [--no-logs]");
    eprintln!("  focaldesk-cli ai providers");
    eprintln!(
        "  focaldesk-cli ai chat [--stream] [--memory] [--provider <id>] [--model <model>] <prompt...>"
    );
    eprintln!("  focaldesk-cli ai ingest <file-or-directory-path> [--recursive]");
    eprintln!("  focaldesk-cli ai sources");
    eprintln!("  focaldesk-cli ai remove-source <canonical-path>");
    eprintln!("  focaldesk-cli ai eval <cases.json> [--top-k <n>]");
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
    citations: Vec<Citation>,
}

fn chat_via_ipc(request: ChatRequest) -> anyhow::Result<AiExecution> {
    match send_ai_request(&AiIpcRequest::Chat { request }) {
        Ok(AiIpcResponse::Chat { response }) => Ok(AiExecution {
            path: "ipc",
            provider: response.provider,
            model: response.model,
            content: response.content,
            citations: response.citations,
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
            AiStreamEvent::Completed { response, .. } => {
                if printed && !ends_with_newline {
                    println!();
                }
                print_citations(&response.citations);
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

fn print_citations(citations: &[Citation]) {
    if citations.is_empty() {
        return;
    }
    eprintln!("[ai] retrieved sources:");
    for (index, citation) in citations.iter().enumerate() {
        eprintln!(
            "  [{}] {} (memory {}, distance {:.4})",
            index + 1,
            citation.source.as_deref().unwrap_or("local memory"),
            citation.memory_id,
            citation.distance
        );
    }
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
    use super::{parse_diagnostics_options, parse_split_keyboard_action, render_ai_output};
    use focaldesk_ipc::DesktopSplitKeyboardAction;
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
    fn split_keyboard_commands_cover_coarse_fine_focus_and_undo() {
        for (name, expected) in [
            ("resize-right", DesktopSplitKeyboardAction::ResizeRight),
            (
                "resize-right-fine",
                DesktopSplitKeyboardAction::ResizeRightFine,
            ),
            ("focus-next", DesktopSplitKeyboardAction::FocusNext),
            ("focus-previous", DesktopSplitKeyboardAction::FocusPrevious),
            ("undo", DesktopSplitKeyboardAction::Undo),
        ] {
            assert_eq!(parse_split_keyboard_action(name).unwrap(), expected);
        }
        assert!(parse_split_keyboard_action("unknown").is_err());
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
