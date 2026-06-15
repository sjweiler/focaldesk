use anyhow::{Context, bail};
use focaldesk_ai::{AiIpcRequest, AiIpcResponse, ChatRequest, send_ai_request};
use focaldesk_ipc::{IpcRequest, IpcResponse, send_desktop_request};

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

            let response = send_desktop_request(&IpcRequest::Notify {
                title,
                body: body_parts.join(" "),
                timeout_ms,
            })
            .map_err(anyhow::Error::msg)?;

            match response {
                IpcResponse::Notification { id } => {
                    println!("notification queued: {id}");
                    Ok(())
                }
                IpcResponse::Ok => Ok(()),
                IpcResponse::Error { message } => bail!(message),
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
        "ai" => handle_ai(args.collect()),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
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
            let mut prompt_parts = Vec::new();

            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--provider" => {
                        provider = Some(args.next().context("--provider requires a value")?);
                    }
                    "--model" => {
                        model = Some(args.next().context("--model requires a value")?);
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

            let response = send_ai_request(&AiIpcRequest::Chat { request })?;
            match response {
                AiIpcResponse::Chat { response } => {
                    println!("{}", response.content);
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
    eprintln!("  focaldesk-cli ai providers");
    eprintln!("  focaldesk-cli ai chat [--provider <id>] [--model <model>] <prompt...>");
}
