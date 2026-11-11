use tauri::Manager;
mod desktop;
mod management;
mod streaming;
use focaldesk_ai::{
    send_ai_request, AiIpcRequest, AiIpcResponse, ChatMessage, ChatRequest, ChatResponse,
    ProviderInfo, ProviderModelInfo,
};

#[tauri::command]
async fn send_message(
    messages: Vec<ChatMessage>,
    provider: Option<String>,
    use_memory: bool,
    model: Option<String>,
) -> Result<ChatResponse, String> {
    if messages.is_empty() || messages.iter().any(|m| m.content.trim().is_empty()) {
        return Err("Messages must contain text".into());
    }
    let request = ChatRequest {
        messages,
        provider,
        model,
        temperature: None,
        max_tokens: None,
        use_memory,
        memory_sources: None,
    };
    tauri::async_runtime::spawn_blocking(move || {
        match send_ai_request(&AiIpcRequest::Chat { request }) {
            Ok(AiIpcResponse::Chat { response }) => Ok(response),
            Ok(AiIpcResponse::Error { message }) => Err(message),
            Ok(_) => Err("Unexpected AI daemon response".into()),
            Err(error) => Err(format!("{error:#}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_providers() -> Result<Vec<ProviderInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| match send_ai_request(&AiIpcRequest::ListProviders) {
        Ok(AiIpcResponse::Providers { providers, .. }) => Ok(providers),
        Ok(AiIpcResponse::Error { message }) => Err(message),
        Ok(_) => Err("Unexpected provider response".into()),
        Err(error) => Err(format!("{error:#}")),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_models(provider: String) -> Result<Vec<ProviderModelInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        match send_ai_request(&AiIpcRequest::ListModels { provider }) {
            Ok(AiIpcResponse::Models { models, .. }) => Ok(models),
            Ok(AiIpcResponse::Error { message }) => Err(message),
            Ok(_) => Err("Unexpected model response".into()),
            Err(error) => Err(format!("{error:#}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// Typed requests retain daemon-side validation, permissions and action confirmation.
async fn request_json(request: AiIpcRequest) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || match send_ai_request(&request) {
        Ok(AiIpcResponse::Error { message }) => Err(message),
        Ok(response) => serde_json::to_value(response).map_err(|e| e.to_string()),
        Err(error) => Err(format!(
            "{error:#}. Verify this console is installed as a trusted FocalDesk IPC client."
        )),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn service_data(section: String) -> Result<serde_json::Value, String> {
    let request = match section.as_str() {
        "status" => AiIpcRequest::Status,
        "memory" => AiIpcRequest::MemoryStatus,
        "documents" => AiIpcRequest::ListIndexedDocuments,
        "agents" => AiIpcRequest::ListAgents,
        "runs" => AiIpcRequest::ListAgentRuns,
        "connectors" => AiIpcRequest::ListConnectors,
        _ => return Err("Unknown service section".into()),
    };
    request_json(request).await
}

#[tauri::command]
async fn search_memory(query: String) -> Result<serde_json::Value, String> {
    if query.trim().is_empty() {
        return Err("Enter a search query".into());
    }
    request_json(AiIpcRequest::Recall { query, top_k: 10 }).await
}

#[tauri::command]
async fn ingest_document(path: String) -> Result<serde_json::Value, String> {
    if !std::path::Path::new(&path).is_absolute() {
        return Err("Enter an absolute document path".into());
    }
    request_json(AiIpcRequest::IngestDocument { path: path.into() }).await
}

#[tauri::command]
async fn ingest_directory(path: String, recursive: bool) -> Result<serde_json::Value, String> {
    if !std::path::Path::new(&path).is_absolute() {
        return Err("Enter an absolute folder path".into());
    }
    request_json(AiIpcRequest::IngestDirectory {
        path: path.into(),
        recursive,
    })
    .await
}

#[tauri::command]
async fn ingest_website(url: String) -> Result<serde_json::Value, String> {
    // The existing daemon validates HTTPS, crawl scope and redirects before fetching.
    request_json(AiIpcRequest::IngestWebsite {
        url: url.trim().into(),
    })
    .await
}

#[tauri::command]
async fn start_agent(agent_id: String, objective: String) -> Result<serde_json::Value, String> {
    if objective.trim().is_empty() {
        return Err("Enter an objective".into());
    }
    request_json(AiIpcRequest::StartAgent {
        request: focaldesk_ai::AgentRequest {
            agent_id: Some(agent_id),
            objective,
            provider: None,
            model: None,
        },
    })
    .await
}

#[tauri::command]
async fn cancel_agent(run_id: String) -> Result<serde_json::Value, String> {
    request_json(AiIpcRequest::CancelAgentRun { run_id }).await
}

#[tauri::command]
async fn set_connector(
    connector_id: String,
    enabled: bool,
    network_allowed: bool,
) -> Result<serde_json::Value, String> {
    request_json(AiIpcRequest::SetConnectorEnabled {
        connector_id,
        enabled,
        network_allowed,
    })
    .await
}

#[tauri::command]
async fn browser_request(
    request: focaldesk_browserd::BrowserRequest,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        focaldesk_browserd::send(&request).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn confirm_agent(plan_id: String, approved: bool) -> Result<serde_json::Value, String> {
    request_json(AiIpcRequest::ConfirmAgentAction { plan_id, approved }).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _telemetry = focaldesk_logging::telemetry::init("focaldesk-ai-console")
        .expect("failed to initialize console telemetry");
    tauri::Builder::default()
        .manage(streaming::Streams::default())
        .manage(desktop::DesktopState::default())
        .invoke_handler(tauri::generate_handler![
            send_message,
            streaming::stream_message,
            streaming::stop_message,
            management::manage_request,
            desktop::load_console_state,
            desktop::save_console_state,
            desktop::legacy_history,
            desktop::pick_source,
            desktop::read_attachment,
            desktop::read_json_file,
            desktop::export_json_file,
            desktop::voice_start,
            desktop::voice_poll,
            desktop::voice_stop,
            desktop::speak,
            desktop::gpu_status,
            desktop::provider_settings,
            desktop::save_provider_settings,
            desktop::restart_ai_backend,
            list_providers,
            list_models,
            service_data,
            search_memory,
            ingest_document,
            ingest_directory,
            ingest_website,
            start_agent,
            cancel_agent,
            set_connector,
            browser_request,
            confirm_agent
        ])
        .on_window_event(|window, event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                let voices = window
                    .state::<desktop::DesktopState>()
                    .voices
                    .lock()
                    .map(|mut voices| voices.drain().collect::<Vec<_>>())
                    .unwrap_or_default();
                let streams = window.state::<streaming::Streams>();
                let ids = streams
                    .0
                    .lock()
                    .map(|streams| {
                        streams
                            .iter()
                            .map(|(id, cancel)| {
                                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                                id.clone()
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                std::thread::spawn(move || {
                    for id in ids {
                        let _ = focaldesk_ai::cancel_ai_stream(&id);
                    }
                    for lease in voices {
                        let _ = focaldesk_ipc::send_microphone_request(
                            &focaldesk_ipc::MicrophoneIpcRequest::Stop {
                                lease_id: Some(lease),
                            },
                        );
                    }
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running FocalDesk AI Console");
}
