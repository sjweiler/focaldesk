//! Typed client for compositor-owned input configuration. No direct-save fallback.
use focaldesk_ipc::{send_desktop_request, InputConfiguration, IpcRequest, IpcResponse};
use focaldesk_settings_core::InputSettings;
fn request(request: IpcRequest) -> Result<InputConfiguration, String> {
    match send_desktop_request(&request)? {
        IpcResponse::InputConfiguration { configuration } => Ok(*configuration),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!(
            "Unexpected input response: {other:?}. Use the compositor from this checkout."
        )),
    }
}
pub fn snapshot() -> Result<InputConfiguration, String> {
    request(IpcRequest::GetInputConfiguration)
}
pub fn apply(
    expected: InputSettings,
    expected_saved: InputSettings,
    input: InputSettings,
) -> Result<InputConfiguration, String> {
    request(IpcRequest::SetInputConfiguration {
        expected: Box::new(expected),
        expected_saved: Box::new(expected_saved),
        input: Box::new(input),
    })
}
