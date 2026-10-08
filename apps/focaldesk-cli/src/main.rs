use anyhow::{Context, bail};
use focaldesk_ai::{
    AgentRequest, AgentRunStatus, AgentTriggerKind, AiIpcRequest, AiIpcResponse, AiStreamEvent,
    ChatRequest, Citation, FaiBundle, FaiForgeProject, FaiLocalRegistry, FaiSigner,
    RetrievalEvalCase, ScenarioFixture, evaluate_scenario, send_ai_request, stream_ai_chat,
};
use focaldesk_diagnostics::{DiagnosticsOptions, collect_diagnostics};
use focaldesk_ipc::{
    DesktopAction, DesktopDirection, DesktopSnapshot, DesktopSplitKeyboardAction,
    DesktopSplitLayout, IpcRequest, IpcResponse, NotificationIpcRequest, NotificationIpcResponse,
    send_desktop_request, send_notification_request,
};
use focaldesk_settings_core::{
    DisplayColorProfile, HdrAppearance, HdrCalibrationPattern, OutputConfig,
};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use zeroize::Zeroizing;

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
        "display-runtime-status" => {
            let response = send_desktop_request(&IpcRequest::GetDisplayRuntimeStatus)
                .map_err(anyhow::Error::msg)?;
            match response {
                IpcResponse::DisplayRuntimeStatus { outputs } => {
                    println!("{}", serde_json::to_string_pretty(&outputs)?);
                    Ok(())
                }
                IpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "hdr-calibration-pattern" => {
            let connector = args
                .next()
                .context("hdr-calibration-pattern requires a connector")?;
            let pattern = parse_hdr_calibration_pattern(
                &args
                    .next()
                    .context("hdr-calibration-pattern requires a pattern")?,
            )?;
            match send_desktop_request(&IpcRequest::SetHdrCalibrationPattern { connector, pattern })
                .map_err(anyhow::Error::msg)?
            {
                IpcResponse::Ok => Ok(()),
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
                    transform: output.transform,
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

fn parse_hdr_calibration_pattern(value: &str) -> anyhow::Result<HdrCalibrationPattern> {
    match value {
        "off" => Ok(HdrCalibrationPattern::Off),
        "overview" => Ok(HdrCalibrationPattern::Overview),
        "near-black" => Ok(HdrCalibrationPattern::NearBlack),
        "reference-white" => Ok(HdrCalibrationPattern::ReferenceWhite),
        "peak-window" => Ok(HdrCalibrationPattern::PeakWindow),
        "peak-full-frame" => Ok(HdrCalibrationPattern::PeakFullFrame),
        other => bail!("unknown HDR calibration pattern `{other}`"),
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
        "scenario" => {
            let path = PathBuf::from(
                args.next()
                    .context("ai scenario requires a JSON fixture path")?,
            );
            if args.next().is_some() {
                bail!("ai scenario accepts exactly one JSON fixture path");
            }
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("inspect scenario fixture {}", path.display()))?;
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 192 * 1024
            {
                bail!("scenario fixture must be a regular non-symlink file no larger than 192 KiB");
            }
            let input = std::fs::read_to_string(&path)
                .with_context(|| format!("read scenario fixture {}", path.display()))?;
            let fixture: ScenarioFixture = serde_json::from_str(&input)
                .with_context(|| format!("invalid Scenario Lab JSON in {}", path.display()))?;
            let report = evaluate_scenario(fixture)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.passed {
                Ok(())
            } else {
                bail!("Scenario Lab contract failed")
            }
        }
        "package" => {
            let action = args.next().context(
                "ai package requires a Forge, registry, inspection, or lifecycle action",
            )?;
            match action.as_str() {
                "init" => {
                    let directory = PathBuf::from(
                        args.next()
                            .context("ai package init requires a new project directory")?,
                    );
                    let mut id = None;
                    let mut name = None;
                    let mut signer = None;
                    while let Some(option) = args.next() {
                        match option.as_str() {
                            "--id" => id = Some(args.next().context("--id requires a value")?),
                            "--name" => {
                                name = Some(args.next().context("--name requires a value")?)
                            }
                            "--signer" => {
                                signer = Some(args.next().context("--signer requires a value")?)
                            }
                            _ => bail!("unknown ai package init option: {option}"),
                        }
                    }
                    let project = FaiForgeProject::example(
                        id.context("ai package init requires --id")?,
                        name.context("ai package init requires --name")?,
                        signer.context("ai package init requires --signer")?,
                    );
                    write_forge_scaffold(&directory, &project)?;
                    println!("created {}", directory.display());
                    Ok(())
                }
                "keygen" => {
                    let signer_id = args
                        .next()
                        .context("ai package keygen requires a signer id")?;
                    if args.next().is_some() {
                        bail!("ai package keygen accepts exactly one signer id");
                    }
                    match send_ai_request(&AiIpcRequest::GeneratePackageSigner { signer_id })? {
                        AiIpcResponse::PackageSignerGenerated { signer } => {
                            println!("{}\t{}", signer.id, signer.public_key_hex);
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "test" => {
                    let project = read_forge_project(&PathBuf::from(
                        args.next()
                            .context("ai package test requires a project path")?,
                    ))?;
                    if args.next().is_some() {
                        bail!("ai package test accepts exactly one project path");
                    }
                    let report = project.test()?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    if report.passed {
                        Ok(())
                    } else {
                        bail!("AIOS package project failed its Scenario Lab suite")
                    }
                }
                "build" | "sign" => {
                    let project_path =
                        PathBuf::from(args.next().with_context(|| {
                            format!("ai package {action} requires a project path")
                        })?);
                    let output =
                        PathBuf::from(args.next().with_context(|| {
                            format!("ai package {action} requires an output path")
                        })?);
                    if args.next().is_some() {
                        bail!("ai package {action} accepts a project path and output path");
                    }
                    let project = read_forge_project(&project_path)?;
                    match send_ai_request(&AiIpcRequest::BuildPackageProject {
                        project: Box::new(project),
                    })? {
                        AiIpcResponse::PackageBuilt { bundle } => {
                            write_new_json(&output, &*bundle)?;
                            println!("built {}", output.display());
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "verify" => {
                    let bundle = read_fai_bundle(&PathBuf::from(
                        args.next()
                            .context("ai package verify requires a .fai path")?,
                    ))?;
                    if args.next().is_some() {
                        bail!("ai package verify accepts exactly one .fai path");
                    }
                    match send_ai_request(&AiIpcRequest::InspectPackage {
                        bundle: Box::new(bundle),
                    })? {
                        AiIpcResponse::PackageInspected { inspection } => {
                            println!("{}", serde_json::to_string_pretty(&inspection)?);
                            if inspection.signature_valid && inspection.scenarios_passed {
                                Ok(())
                            } else {
                                bail!("AIOS package verification failed")
                            }
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "registry" => handle_fai_registry(args.collect()),
                "inspect" | "stage" => {
                    let path =
                        PathBuf::from(args.next().with_context(|| {
                            format!("ai package {action} requires a .fai path")
                        })?);
                    if args.next().is_some() {
                        bail!("ai package {action} accepts exactly one .fai path");
                    }
                    let bundle = read_fai_bundle(&path)?;
                    let request = if action == "inspect" {
                        AiIpcRequest::InspectPackage {
                            bundle: Box::new(bundle),
                        }
                    } else {
                        AiIpcRequest::StagePackage {
                            bundle: Box::new(bundle),
                        }
                    };
                    match send_ai_request(&request)? {
                        AiIpcResponse::PackageInspected { inspection } => {
                            println!("{}", serde_json::to_string_pretty(&inspection)?);
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "trust" => {
                    let id = args
                        .next()
                        .context("ai package trust requires a signer id")?;
                    let public_key_hex = args
                        .next()
                        .context("ai package trust requires an Ed25519 public key")?;
                    if args.next().is_some() {
                        bail!("ai package trust accepts a signer id and public key");
                    }
                    match send_ai_request(&AiIpcRequest::TrustPackageSigner {
                        signer: FaiSigner { id, public_key_hex },
                    })? {
                        AiIpcResponse::PackageSignerTrusted => {
                            println!("package signer trusted");
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "activate" | "rollback" => {
                    let package_id = args
                        .next()
                        .with_context(|| format!("ai package {action} requires a package id"))?;
                    if args.next().is_some() {
                        bail!("ai package {action} accepts exactly one package id");
                    }
                    let request = if action == "activate" {
                        AiIpcRequest::ActivatePackage { package_id }
                    } else {
                        AiIpcRequest::RollbackPackage { package_id }
                    };
                    match send_ai_request(&request)? {
                        AiIpcResponse::PackageActivated { bundle } => {
                            println!(
                                "{}\t{}\t{}",
                                bundle.manifest.id, bundle.manifest.version, action
                            );
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                "list" => {
                    if args.next().is_some() {
                        bail!("ai package list does not accept arguments");
                    }
                    match send_ai_request(&AiIpcRequest::ListPackages)? {
                        AiIpcResponse::Packages { packages } => {
                            println!("{}", serde_json::to_string_pretty(&packages)?);
                            Ok(())
                        }
                        AiIpcResponse::Error { message } => bail!(message),
                        other => bail!("unexpected AI response: {other:?}"),
                    }
                }
                other => bail!("unknown ai package action: {other}"),
            }
        }
        "agents" => {
            if args.next().is_some() {
                bail!("ai agents does not accept arguments");
            }
            match send_ai_request(&AiIpcRequest::ListAgents)? {
                AiIpcResponse::Agents { agents } => {
                    for agent in agents {
                        println!(
                            "{}\t{}\tvoice={}\tsteps={}\ttriggers={}\t{}",
                            agent.id,
                            agent.name,
                            agent.voice,
                            agent.max_tool_steps,
                            agent.triggers.len(),
                            agent.description
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-runs" => {
            if args.next().is_some() {
                bail!("ai agent-runs does not accept arguments");
            }
            match send_ai_request(&AiIpcRequest::ListAgentRuns)? {
                AiIpcResponse::AgentRuns { runs } => {
                    for run in runs {
                        print_agent_run(&run);
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-status" => {
            let run_id = args.next().context("ai agent-status requires a run id")?;
            if args.next().is_some() {
                bail!("ai agent-status accepts exactly one run id");
            }
            match send_ai_request(&AiIpcRequest::GetAgentRun {
                run_id: run_id.clone(),
            })? {
                AiIpcResponse::AgentRun {
                    status: Some(status),
                    ..
                } => {
                    print_agent_run(&status);
                    Ok(())
                }
                AiIpcResponse::AgentRun { status: None, .. } => {
                    bail!("unknown agent run: {run_id}")
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-cancel" => {
            let run_id = args.next().context("ai agent-cancel requires a run id")?;
            if args.next().is_some() {
                bail!("ai agent-cancel accepts exactly one run id");
            }
            match send_ai_request(&AiIpcRequest::CancelAgentRun {
                run_id: run_id.clone(),
            })? {
                AiIpcResponse::AgentRunCancellation { accepted, .. } => {
                    if accepted {
                        println!("cancelled {run_id}");
                        Ok(())
                    } else {
                        bail!("agent run is unknown or no longer cancellable: {run_id}")
                    }
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-retry" => {
            let run_id = args.next().context("ai agent-retry requires a run id")?;
            if args.next().is_some() {
                bail!("ai agent-retry accepts exactly one run id");
            }
            match send_ai_request(&AiIpcRequest::RetryAgentRun { run_id })? {
                AiIpcResponse::AgentStarted { run_id } => {
                    println!("retried as {run_id}");
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-trigger" => {
            let agent_id = args
                .next()
                .context("ai agent-trigger requires an agent id")?;
            let trigger_id = args
                .next()
                .context("ai agent-trigger requires a trigger id")?;
            if args.next().is_some() {
                bail!("ai agent-trigger accepts exactly an agent id and trigger id");
            }
            match send_ai_request(&AiIpcRequest::FireAgentTrigger {
                agent_id,
                trigger_id,
            })? {
                AiIpcResponse::AgentStarted { run_id } => {
                    println!("triggered {run_id}");
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-event" => {
            let kind = parse_agent_trigger_kind(
                &args
                    .next()
                    .context("ai agent-event requires an event kind")?,
            )?;
            let value = args.collect::<Vec<_>>().join(" ");
            if value.is_empty() {
                bail!("ai agent-event requires an event value");
            }
            match send_ai_request(&AiIpcRequest::DispatchAgentEvent { kind, value })? {
                AiIpcResponse::AgentTriggersStarted { run_ids } => {
                    for run_id in run_ids {
                        println!("triggered {run_id}");
                    }
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-triggers" => {
            let action = args.next().unwrap_or_else(|| "status".into());
            if args.next().is_some() {
                bail!("ai agent-triggers accepts status, suspend, or resume");
            }
            let request = match action.as_str() {
                "status" => AiIpcRequest::GetAgentTriggerState,
                "suspend" => AiIpcRequest::SetAgentTriggersSuspended { suspended: true },
                "resume" => AiIpcRequest::SetAgentTriggersSuspended { suspended: false },
                _ => bail!("ai agent-triggers accepts status, suspend, or resume"),
            };
            match send_ai_request(&request)? {
                AiIpcResponse::AgentTriggerState { suspended } => {
                    println!(
                        "agent triggers: {}",
                        if suspended { "suspended" } else { "active" }
                    );
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent-control" => {
            let action = args.next().unwrap_or_else(|| "status".into());
            let agent_id = args.next();
            if args.next().is_some() {
                bail!("ai agent-control accepts an action and optional agent id");
            }
            let request = match action.as_str() {
                "status" => AiIpcRequest::GetAgentControlStatuses,
                "reload" => AiIpcRequest::ReloadAgents,
                "enable" => AiIpcRequest::SetAgentEnabled {
                    agent_id: agent_id.context("enable requires an agent id")?,
                    enabled: true,
                },
                "disable" => AiIpcRequest::SetAgentEnabled {
                    agent_id: agent_id.context("disable requires an agent id")?,
                    enabled: false,
                },
                "rollback" => AiIpcRequest::RollbackAgent {
                    agent_id: agent_id.context("rollback requires an agent id")?,
                },
                _ => bail!("agent-control accepts status, reload, enable, disable, or rollback"),
            };
            match send_ai_request(&request)? {
                AiIpcResponse::AgentControlStatuses { agents } => {
                    for agent in agents {
                        println!(
                            "{}\t{}\truns={}\tfailures={}\ttokens={}\tcost_microusd={}",
                            agent.definition.id,
                            if agent.enabled { "enabled" } else { "disabled" },
                            agent.runs_today,
                            agent.failures_today,
                            agent
                                .input_tokens_today
                                .saturating_add(agent.output_tokens_today),
                            agent.estimated_cost_microusd_today,
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::Agents { agents } => {
                    println!("loaded {} agents", agents.len());
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "workflow" => {
            let action = args.next().unwrap_or_else(|| "list".into());
            let value = args.next();
            if args.next().is_some() {
                bail!("ai workflow accepts one action and optional id");
            }
            let request = match action.as_str() {
                "list" => AiIpcRequest::ListWorkflows,
                "runs" => AiIpcRequest::ListWorkflowRuns,
                "start" => AiIpcRequest::StartWorkflow {
                    workflow_id: value.context("workflow start requires a workflow id")?,
                },
                "status" => AiIpcRequest::GetWorkflowRun {
                    run_id: value.context("workflow status requires a run id")?,
                },
                "pause" | "resume" => AiIpcRequest::SetWorkflowPaused {
                    run_id: value.context("workflow pause/resume requires a run id")?,
                    paused: action == "pause",
                },
                "cancel" => AiIpcRequest::CancelWorkflow {
                    run_id: value.context("workflow cancel requires a run id")?,
                },
                "retry" => AiIpcRequest::RetryWorkflow {
                    run_id: value.context("workflow retry requires a run id")?,
                },
                _ => bail!(
                    "workflow accepts list, runs, start, status, pause, resume, cancel, or retry"
                ),
            };
            match send_ai_request(&request)? {
                AiIpcResponse::Workflows { workflows } => {
                    for workflow in workflows {
                        println!(
                            "{}\t{}\tnodes={}\tparallel={}\ttokens={}\t{}",
                            workflow.id,
                            workflow.name,
                            workflow.nodes.len(),
                            workflow.max_parallelism,
                            workflow.max_total_tokens,
                            workflow.description
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::WorkflowRuns { runs } => {
                    for run in runs {
                        println!(
                            "{}\t{}\t{:?}\ttokens={}/{}\tnodes={}",
                            run.run_id,
                            run.workflow_id,
                            run.state,
                            run.total_tokens,
                            run.max_total_tokens,
                            run.nodes.len()
                        );
                    }
                    Ok(())
                }
                AiIpcResponse::WorkflowRun { status, .. } => {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                    Ok(())
                }
                AiIpcResponse::WorkflowStarted { run_id } => {
                    println!("workflow started {run_id}");
                    Ok(())
                }
                AiIpcResponse::WorkflowControl { run_id, accepted } => {
                    println!(
                        "workflow {run_id}: {}",
                        if accepted { "accepted" } else { "unchanged" }
                    );
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "capabilities" => {
            let action = args.next().unwrap_or_else(|| "leases".into());
            let value = args.next();
            if args.next().is_some() {
                bail!("ai capabilities accepts one action and optional id");
            }
            let request = match action.as_str() {
                "preview" => AiIpcRequest::PreviewCapabilities {
                    agent_id: value.context("capabilities preview requires an agent id")?,
                    ceiling: None,
                },
                "leases" => AiIpcRequest::ListCapabilityLeases,
                "revoke" => AiIpcRequest::RevokeCapabilityLease {
                    lease_id: value.context("capabilities revoke requires a lease id")?,
                },
                _ => bail!("capabilities accepts preview, leases, or revoke"),
            };
            match send_ai_request(&request)? {
                AiIpcResponse::CapabilityPreview { preview } => {
                    println!("{}", serde_json::to_string_pretty(&preview)?);
                    Ok(())
                }
                AiIpcResponse::CapabilityLeases { leases } => {
                    println!("{}", serde_json::to_string_pretty(&leases)?);
                    Ok(())
                }
                AiIpcResponse::CapabilityRevocation { lease_id, revoked } => {
                    println!("{lease_id}\trevoked={revoked}");
                    Ok(())
                }
                AiIpcResponse::Error { message } => bail!(message),
                other => bail!("unexpected AI response: {other:?}"),
            }
        }
        "agent" => {
            let mut agent_id = None;
            let mut provider = None;
            let mut model = None;
            let mut objective_parts = Vec::new();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--profile" => {
                        agent_id = Some(args.next().context("--profile requires a value")?);
                    }
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
                    agent_id,
                    provider,
                    model,
                },
            })? {
                AiIpcResponse::Agent { response } => {
                    eprintln!(
                        "[ai-agent] run={} provider={} model={} tools={}",
                        response.run_id,
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

fn read_fai_bundle(path: &std::path::Path) -> anyhow::Result<FaiBundle> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect AIOS package {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 2 * 1024 * 1024
    {
        bail!("AIOS package must be a regular non-symlink file no larger than 2 MiB");
    }
    let input = std::fs::read_to_string(path)
        .with_context(|| format!("read AIOS package {}", path.display()))?;
    serde_json::from_str(&input).with_context(|| format!("invalid .fai JSON in {}", path.display()))
}

fn read_forge_project(path: &std::path::Path) -> anyhow::Result<FaiForgeProject> {
    let path = if path.is_dir() {
        path.join("fai-project.json")
    } else {
        path.to_path_buf()
    };
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("inspect AIOS Forge project {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 2 * 1024 * 1024
    {
        bail!("Forge project must be a regular non-symlink file no larger than 2 MiB");
    }
    serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("invalid Forge project JSON in {}", path.display()))
}

fn write_forge_scaffold(
    directory: &std::path::Path,
    project: &FaiForgeProject,
) -> anyhow::Result<()> {
    if directory.exists() {
        bail!("Forge project directory already exists");
    }
    std::fs::create_dir(directory)
        .with_context(|| format!("create Forge project directory {}", directory.display()))?;
    write_new_json(&directory.join("fai-project.json"), project)?;
    let readme_path = directory.join("README.md");
    let mut readme = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&readme_path)?;
    writeln!(
        readme,
        "# {}\n\nTest and build this package with:\n\n```text\nfocaldesk-cli ai package test .\nfocaldesk-cli ai package build . {}.fai\n```",
        project.manifest.name, project.manifest.id
    )?;
    readme.sync_all()?;
    Ok(())
}

fn write_new_json(path: &std::path::Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    if path.exists() {
        bail!("refusing to overwrite existing file: {}", path.display());
    }
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    file.sync_all()?;
    Ok(())
}

fn handle_fai_registry(args: Vec<String>) -> anyhow::Result<()> {
    let mut args = args.into_iter();
    let action = args
        .next()
        .context("ai package registry requires a local or remote action")?;
    let root = std::env::var_os("FOCALDESK_FAI_REGISTRY")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|path| path.join("focaldesk/fai-registry")))
        .context("cannot resolve local AIOS registry path")?;
    let registry = FaiLocalRegistry::open(root)?;
    match action.as_str() {
        "add" => {
            let bundle_path = PathBuf::from(
                args.next()
                    .context("ai package registry add requires a .fai path")?,
            );
            let mut overwrite = false;
            for option in args {
                match option.as_str() {
                    "--overwrite" => overwrite = true,
                    _ => bail!("unknown registry add option: {option}"),
                }
            }
            let entry = registry.add(&read_fai_bundle(&bundle_path)?, overwrite)?;
            println!("{}", serde_json::to_string_pretty(&entry)?);
            Ok(())
        }
        "list" => {
            if args.next().is_some() {
                bail!("ai package registry list does not accept arguments");
            }
            println!("{}", serde_json::to_string_pretty(&registry.list(None)?)?);
            Ok(())
        }
        "search" => {
            let query = args
                .next()
                .context("ai package registry search requires a query")?;
            if args.next().is_some() {
                bail!("ai package registry search accepts exactly one query");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&registry.list(Some(&query))?)?
            );
            Ok(())
        }
        "sync" => {
            let base_url = args.next().context("registry sync requires a base URL")?;
            let catalog_key = args
                .next()
                .context("registry sync requires a pinned catalog public key")?;
            let token = read_private_token(&PathBuf::from(
                args.next().context("registry sync requires a token file")?,
            ))?;
            let output = PathBuf::from(
                args.next()
                    .context("registry sync requires a catalog output path")?,
            );
            if args.next().is_some() {
                bail!("registry sync received too many arguments");
            }
            let previous = if output.exists() {
                let prior = read_signed_catalog(&output)?;
                focaldesk_ai::verify_registry_catalog(&prior, &catalog_key, None)?;
                Some(prior.catalog.sequence)
            } else {
                None
            };
            let catalog = tokio::runtime::Runtime::new()?.block_on(
                focaldesk_ai::fetch_registry_catalog(&base_url, &token, &catalog_key, previous),
            )?;
            write_json_atomic(&output, &catalog)?;
            println!(
                "registry={} sequence={} packages={}",
                catalog.catalog.registry_id,
                catalog.catalog.sequence,
                catalog.catalog.packages.len()
            );
            Ok(())
        }
        "browse" => {
            let catalog_path = PathBuf::from(
                args.next()
                    .context("registry browse requires a catalog path")?,
            );
            let catalog_key = args
                .next()
                .context("registry browse requires a pinned catalog public key")?;
            let query = args.next().map(|value| value.to_ascii_lowercase());
            if args.next().is_some() {
                bail!("registry browse accepts at most one query");
            }
            let catalog = read_signed_catalog(&catalog_path)?;
            focaldesk_ai::verify_registry_catalog(&catalog, &catalog_key, None)?;
            let entries = catalog
                .catalog
                .packages
                .into_iter()
                .filter(|entry| {
                    query.as_ref().is_none_or(|query| {
                        entry.package_id.to_ascii_lowercase().contains(query)
                            || entry.name.to_ascii_lowercase().contains(query)
                    })
                })
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string_pretty(&entries)?);
            Ok(())
        }
        "lock" => {
            let catalog_path = PathBuf::from(
                args.next()
                    .context("registry lock requires a catalog path")?,
            );
            let catalog_key = args
                .next()
                .context("registry lock requires a pinned catalog public key")?;
            let package_id = args.next().context("registry lock requires a package id")?;
            let version = args.next().context("registry lock requires a version")?;
            let output = PathBuf::from(
                args.next()
                    .context("registry lock requires an output path")?,
            );
            if args.next().is_some() {
                bail!("registry lock received too many arguments");
            }
            let catalog = read_signed_catalog(&catalog_path)?;
            focaldesk_ai::verify_registry_catalog(&catalog, &catalog_key, None)?;
            write_new_json(
                &output,
                &focaldesk_ai::resolve_catalog_lock(&catalog, &package_id, &version)?,
            )?;
            println!("wrote {}", output.display());
            Ok(())
        }
        "diff" => {
            let catalog_path = PathBuf::from(
                args.next()
                    .context("registry diff requires a catalog path")?,
            );
            let catalog_key = args
                .next()
                .context("registry diff requires a pinned catalog public key")?;
            let package_id = args.next().context("registry diff requires a package id")?;
            let from_version = args
                .next()
                .context("registry diff requires a from version")?;
            let to_version = args.next().context("registry diff requires a to version")?;
            if args.next().is_some() {
                bail!("registry diff received too many arguments");
            }
            let catalog = read_signed_catalog(&catalog_path)?;
            focaldesk_ai::verify_registry_catalog(&catalog, &catalog_key, None)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&focaldesk_ai::compare_catalog_versions(
                    &catalog,
                    &package_id,
                    &from_version,
                    &to_version,
                )?)?
            );
            Ok(())
        }
        "download" => {
            let base_url = args
                .next()
                .context("registry download requires a base URL")?;
            let catalog_key = args
                .next()
                .context("registry download requires a pinned catalog public key")?;
            let token = read_private_token(&PathBuf::from(
                args.next()
                    .context("registry download requires a token file")?,
            ))?;
            let catalog_path = PathBuf::from(
                args.next()
                    .context("registry download requires a catalog path")?,
            );
            let package_id = args
                .next()
                .context("registry download requires a package id")?;
            let version = args
                .next()
                .context("registry download requires a version")?;
            let output = PathBuf::from(
                args.next()
                    .context("registry download requires a quarantine output path")?,
            );
            if args.next().is_some() {
                bail!("registry download received too many arguments");
            }
            let catalog = read_signed_catalog(&catalog_path)?;
            focaldesk_ai::verify_registry_catalog(&catalog, &catalog_key, None)?;
            let bundle = tokio::runtime::Runtime::new()?.block_on(
                focaldesk_ai::download_registry_package(
                    &base_url,
                    &token,
                    &catalog,
                    &package_id,
                    &version,
                ),
            )?;
            write_new_json(&output, &bundle)?;
            println!("verified download quarantined at {}", output.display());
            Ok(())
        }
        "publish" => {
            let base_url = args
                .next()
                .context("registry publish requires a base URL")?;
            let token = read_private_token(&PathBuf::from(
                args.next()
                    .context("registry publish requires a token file")?,
            ))?;
            let bundle = read_fai_bundle(&PathBuf::from(
                args.next()
                    .context("registry publish requires a .fai path")?,
            ))?;
            if args.next().is_some() {
                bail!("registry publish received too many arguments");
            }
            let entry = tokio::runtime::Runtime::new()?.block_on(
                focaldesk_ai::publish_registry_package(&base_url, &token, &bundle),
            )?;
            println!("{}", serde_json::to_string_pretty(&entry)?);
            Ok(())
        }
        "approve" => {
            let base_url = args
                .next()
                .context("registry approve requires a base URL")?;
            let token = read_private_token(&PathBuf::from(
                args.next()
                    .context("registry approve requires a publish-token file")?,
            ))?;
            let signer = FaiSigner {
                id: args
                    .next()
                    .context("registry approve requires a signer id")?,
                public_key_hex: args
                    .next()
                    .context("registry approve requires a signer public key")?,
            };
            if args.next().is_some() {
                bail!("registry approve received too many arguments");
            }
            let policy = tokio::runtime::Runtime::new()?.block_on(
                focaldesk_ai::approve_registry_signer(&base_url, &token, &signer),
            )?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
            Ok(())
        }
        "revoke" => {
            let base_url = args.next().context("registry revoke requires a base URL")?;
            let token = read_private_token(&PathBuf::from(
                args.next()
                    .context("registry revoke requires a publish-token file")?,
            ))?;
            let package_id = args
                .next()
                .context("registry revoke requires a package id")?;
            let version = args.next().context("registry revoke requires a version")?;
            let reason = args.collect::<Vec<_>>().join(" ");
            let revocation =
                tokio::runtime::Runtime::new()?.block_on(focaldesk_ai::revoke_registry_package(
                    &base_url,
                    &token,
                    &package_id,
                    &version,
                    &reason,
                ))?;
            println!("{}", serde_json::to_string_pretty(&revocation)?);
            Ok(())
        }
        _ => bail!("unknown ai package registry action: {action}"),
    }
}

fn read_private_token(path: &std::path::Path) -> anyhow::Result<Zeroizing<String>> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > 4096
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("registry token must be a private regular file no larger than 4 KiB");
    }
    Ok(Zeroizing::new(
        std::fs::read_to_string(path)?.trim().to_string(),
    ))
}

fn read_signed_catalog(path: &std::path::Path) -> anyhow::Result<focaldesk_ai::FaiSignedCatalog> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 4 * 1024 * 1024
    {
        bail!("registry catalog must be a regular file no larger than 4 MiB");
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn write_json_atomic(path: &std::path::Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("output file name is invalid")?;
    let temp = parent.join(format!(".{name}.sync.{}.tmp", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        writeln!(file)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  focaldesk-cli notify <title> [body...] [--timeout-ms <ms>]");
    eprintln!("  focaldesk-cli identify-displays");
    eprintln!("  focaldesk-cli desktop-snapshot");
    eprintln!("  focaldesk-cli display-runtime-status");
    eprintln!(
        "  focaldesk-cli hdr-calibration-pattern <connector> <off|overview|near-black|reference-white|peak-window|peak-full-frame>"
    );
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
    eprintln!("  focaldesk-cli ai scenario <fixture.json>");
    eprintln!("  focaldesk-cli ai package inspect <bundle.fai>");
    eprintln!("  focaldesk-cli ai package init <directory> --id <id> --name <name> --signer <id>");
    eprintln!("  focaldesk-cli ai package keygen <signer-id>");
    eprintln!("  focaldesk-cli ai package test <project-directory-or-json>");
    eprintln!("  focaldesk-cli ai package build <project> <bundle.fai>");
    eprintln!("  focaldesk-cli ai package sign <project> <bundle.fai>");
    eprintln!("  focaldesk-cli ai package verify <bundle.fai>");
    eprintln!("  focaldesk-cli ai package registry add <bundle.fai> [--overwrite]");
    eprintln!("  focaldesk-cli ai package registry list");
    eprintln!("  focaldesk-cli ai package registry search <query>");
    eprintln!(
        "  focaldesk-cli ai package registry sync <url> <catalog-key> <token-file> <catalog.json>"
    );
    eprintln!("  focaldesk-cli ai package registry browse <catalog.json> <catalog-key> [query]");
    eprintln!(
        "  focaldesk-cli ai package registry lock <catalog.json> <catalog-key> <id> <version> <lock.json>"
    );
    eprintln!(
        "  focaldesk-cli ai package registry diff <catalog.json> <catalog-key> <id> <from> <to>"
    );
    eprintln!(
        "  focaldesk-cli ai package registry download <url> <catalog-key> <token-file> <catalog.json> <id> <version> <output.fai>"
    );
    eprintln!("  focaldesk-cli ai package registry publish <url> <token-file> <bundle.fai>");
    eprintln!(
        "  focaldesk-cli ai package registry approve <url> <publish-token-file> <signer-id> <public-key>"
    );
    eprintln!(
        "  focaldesk-cli ai package registry revoke <url> <publish-token-file> <id> <version> <reason...>"
    );
    eprintln!("  focaldesk-cli ai package trust <signer-id> <public-key-hex>");
    eprintln!("  focaldesk-cli ai package stage <bundle.fai>");
    eprintln!("  focaldesk-cli ai package activate <package-id>");
    eprintln!("  focaldesk-cli ai package rollback <package-id>");
    eprintln!("  focaldesk-cli ai package list");
    eprintln!("  focaldesk-cli ai agents");
    eprintln!(
        "  focaldesk-cli ai agent [--profile <id>] [--provider <id>] [--model <model>] <objective...>"
    );
    eprintln!("  focaldesk-cli ai agent-runs");
    eprintln!("  focaldesk-cli ai agent-status <run-id>");
    eprintln!("  focaldesk-cli ai agent-cancel <run-id>");
    eprintln!("  focaldesk-cli ai agent-retry <run-id>");
    eprintln!("  focaldesk-cli ai agent-trigger <agent-id> <trigger-id>");
    eprintln!(
        "  focaldesk-cli ai agent-event <desktop_event|voice_phrase|hotkey|ipc_event> <value>"
    );
    eprintln!("  focaldesk-cli ai agent-triggers [status|suspend|resume]");
    eprintln!(
        "  focaldesk-cli ai agent-control [status|reload|enable <id>|disable <id>|rollback <id>]"
    );
    eprintln!(
        "  focaldesk-cli ai workflow [list|runs|start <id>|status <run>|pause <run>|resume <run>|cancel <run>|retry <run>]"
    );
    eprintln!("  focaldesk-cli ai capabilities [preview <agent-id>|leases|revoke <lease-id>]");
    eprintln!("  focaldesk-cli ai confirm <plan-id>");
    eprintln!("  focaldesk-cli ai deny <plan-id>");
}

fn print_agent_run(run: &AgentRunStatus) {
    println!(
        "{}\t{}\tprovider={}\tsteps={}/{}\tcontext={}\toutput_tokens={}\t{}",
        run.run_id,
        run.state.as_str(),
        run.provider,
        run.completed_tool_steps,
        run.max_tool_steps,
        run.max_context_chars,
        run.max_output_tokens,
        run.objective_preview
    );
    if let Some(error) = run.error.as_deref() {
        eprintln!("[ai-agent] {}: {error}", run.run_id);
    }
}

fn parse_agent_trigger_kind(value: &str) -> anyhow::Result<AgentTriggerKind> {
    match value {
        "desktop_event" => Ok(AgentTriggerKind::DesktopEvent),
        "voice_phrase" => Ok(AgentTriggerKind::VoicePhrase),
        "hotkey" => Ok(AgentTriggerKind::Hotkey),
        "ipc_event" => Ok(AgentTriggerKind::IpcEvent),
        "schedule" => bail!("schedule events are dispatched by the daemon"),
        other => bail!("unknown agent event kind: {other}"),
    }
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
    use super::{
        parse_diagnostics_options, parse_hdr_calibration_pattern, parse_split_keyboard_action,
        render_ai_output,
    };
    use focaldesk_ipc::DesktopSplitKeyboardAction;
    use focaldesk_settings_core::HdrCalibrationPattern;
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
    fn hdr_calibration_pattern_names_are_explicit() {
        assert_eq!(
            parse_hdr_calibration_pattern("reference-white").unwrap(),
            HdrCalibrationPattern::ReferenceWhite
        );
        assert_eq!(
            parse_hdr_calibration_pattern("peak-window").unwrap(),
            HdrCalibrationPattern::PeakWindow
        );
        assert!(parse_hdr_calibration_pattern("white").is_err());
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
