//! Launch compositor-owned application selection from a terminal or launcher.
use focaldesk_ipc::{DesktopAction, IpcRequest, IpcResponse, send_desktop_request};

fn main() -> anyhow::Result<()> {
    match send_desktop_request(&IpcRequest::ExecuteDesktopAction {
        action: DesktopAction::BeginKillSelection,
    })
    .map_err(anyhow::Error::msg)?
    {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}
