use anyhow::{Context, Result};
use focaldesk_ipc::{send_desktop_request, DesktopAction, IpcRequest, IpcResponse};

pub struct IpcClient;

impl IpcClient {
    pub fn connect() -> Result<Self> {
        Ok(Self)
    }

    pub fn send(&mut self, action: DesktopAction) -> Result<()> {
        match send_desktop_request(&IpcRequest::ExecuteDesktopAction { action })
            .map_err(anyhow::Error::msg)
            .context("sending action to compositor")?
        {
            IpcResponse::Ok => Ok(()),
            IpcResponse::Error { message } => Err(anyhow::Error::msg(message)),
            response => anyhow::bail!("unexpected compositor response: {response:?}"),
        }
    }
}
