use focaldesk_ai::AiIpcRequest;
#[tauri::command]
pub async fn manage_request(request: AiIpcRequest) -> Result<serde_json::Value, String> {
    if matches!(
        request,
        AiIpcRequest::Chat { .. }
            | AiIpcRequest::ChatStream { .. }
            | AiIpcRequest::CancelStream { .. }
    ) {
        return Err("Use the chat transport for generation".into());
    }
    super::request_json(request).await
}
