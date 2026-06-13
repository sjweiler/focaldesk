use anyhow::{Context, bail};
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
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  focaldesk-cli notify <title> [body...] [--timeout-ms <ms>]");
    eprintln!("  focaldesk-cli identify-displays");
}
