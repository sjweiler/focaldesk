use focaldesk_ai::{cancel_ai_stream, stream_ai_chat_with_id, AiStreamEvent, ChatRequest};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::ipc::Channel;
#[derive(Default)]
pub struct Streams(pub Mutex<HashMap<String, Arc<AtomicBool>>>);
#[tauri::command]
pub async fn stream_message(
    request: ChatRequest,
    request_id: String,
    on_event: Channel<AiStreamEvent>,
    state: tauri::State<'_, Streams>,
) -> Result<(), String> {
    if request.messages.is_empty() {
        return Err("Enter a message".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut streams = state.0.lock().map_err(|e| e.to_string())?;
        if streams.contains_key(&request_id) {
            return Err("Request is already running".into());
        }
        streams.insert(request_id.clone(), cancel.clone());
    }
    let id = request_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        stream_ai_chat_with_id(request, id.clone(), move |event| {
            if cancel.load(Ordering::SeqCst) && matches!(event, AiStreamEvent::Started { .. }) {
                let _ = cancel_ai_stream(&id);
            }
            on_event
                .send(event)
                .map_err(|e| anyhow::anyhow!(e.to_string()))
        })
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string());
    state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&request_id);
    result?
}
#[tauri::command]
pub async fn stop_message(
    request_id: String,
    state: tauri::State<'_, Streams>,
) -> Result<bool, String> {
    if let Some(cancel) = state.0.lock().map_err(|e| e.to_string())?.get(&request_id) {
        cancel.store(true, Ordering::SeqCst);
    }
    tauri::async_runtime::spawn_blocking(move || {
        cancel_ai_stream(&request_id).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
