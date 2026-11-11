//! Isolated IPC/workflow test. Run with FOCALDESK_AI_PERMISSION=allow-session;
//! every document and vector belongs to this temporary fixture, not the user index.
use anyhow::Result;
use async_trait::async_trait;
use focaldesk_ai::ipc::{send_ai_request_at, serve_ai_ipc_at, stream_ai_chat_at};
use focaldesk_ai::provider::AiProvider;
use focaldesk_ai::{
    AiIpcRequest, AiIpcResponse, AiService, AiStreamEvent, ChatRequest, ChatResponse,
    IngestionSource, ProviderInfo, ProviderModelInfo,
};
use focaldesk_memory::{EmbeddingProvider, MemoryService, MemoryStore};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
struct Embedder {
    gate: AtomicBool,
    started: Notify,
    release: Notify,
}
#[async_trait]
impl EmbeddingProvider for Embedder {
    fn dimension(&self) -> usize {
        3
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Ok(vec![1., 0., 0.])
    }
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.gate.swap(false, Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(texts.iter().map(|_| vec![1., 0., 0.]).collect())
    }
}
struct Provider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
#[async_trait]
impl AiProvider for Provider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            kind: "fixture".into(),
            base_url: None,
            default_model: Some("fixture".into()),
        }
    }
    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>> {
        Ok(vec![])
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(ChatResponse {
            provider: "fixture".into(),
            model: Some("fixture".into()),
            content: "The build channel is cyan [source 1].".into(),
            citations: vec![],
            usage: None,
        })
    }
    async fn chat_stream(
        &self,
        request: ChatRequest,
        deltas: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        if request
            .messages
            .iter()
            .any(|m| m.content == "split secret fixture")
        {
            let mut response = self.chat(request).await?;
            for chunk in [
                "## Safe heading\n",
                "password: syn",
                "thetic-secret\n",
                "Contact person@",
                "example.com\n",
            ] {
                deltas.send(chunk.into()).await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            response.content =
                "## Safe heading\npassword: synthetic-secret\nContact person@example.com\n".into();
            return Ok(response);
        }
        let slow = request
            .messages
            .iter()
            .any(|m| m.content == "slow stream fixture");
        let result = self.chat(request).await?;
        deltas.send("The build channel ".into()).await?;
        if slow {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        deltas.send("is cyan.".into()).await?;
        Ok(result)
    }
}
async fn rpc(socket: &std::path::Path, request: AiIpcRequest) -> AiIpcResponse {
    let socket = socket.to_owned();
    tokio::task::spawn_blocking(move || send_ai_request_at(socket, &request))
        .await
        .unwrap()
        .unwrap()
}
async fn finished(service: &AiService, id: &str) -> focaldesk_ai::IngestionJob {
    for _ in 0..300 {
        let job = service.ingestion_job(id).unwrap();
        if matches!(job.state.as_str(), "completed" | "failed" | "cancelled") {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fixture job did not finish")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run with FOCALDESK_AI_PERMISSION=allow-session against isolated fixture storage"]
async fn indexing_scope_streaming_and_stop_use_real_ipc() {
    assert_eq!(
        std::env::var("FOCALDESK_AI_PERMISSION").as_deref(),
        Ok("allow-session")
    );
    let root = std::env::temp_dir().join(format!(
        "focaldesk-console-parity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("docs/nested")).unwrap();
    std::fs::write(
        root.join("docs/alpha.md"),
        "FocalDesk alpha retrieval fixture. The project's build channel is cyan.",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/nested/beta.md"),
        "FocalDesk beta retrieval fixture. The project's build channel is amber.",
    )
    .unwrap();
    let embedder = Arc::new(Embedder {
        gate: AtomicBool::new(false),
        started: Notify::new(),
        release: Notify::new(),
    });
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut service = AiService::new("fixture").with_memory(MemoryService::new(
        MemoryStore::open(root.join("memory.db"), 3).unwrap(),
        embedder.clone(),
    ));
    service.register(Arc::new(Provider {
        requests: requests.clone(),
    }));
    let service = Arc::new(service);
    let socket = root.join("ai.sock");
    let server_service = service.clone();
    let server_socket = socket.clone();
    let server = tokio::spawn(async move {
        serve_ai_ipc_at(server_service, server_socket)
            .await
            .unwrap();
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let job = match rpc(
        &socket,
        AiIpcRequest::StartIngestion {
            source: IngestionSource::Directory {
                path: root.join("docs"),
                recursive: true,
            },
        },
    )
    .await
    {
        AiIpcResponse::IngestionJob { job } => job,
        response => panic!("{response:?}"),
    };
    let completed = finished(&service, &job.id).await;
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.result.as_ref().unwrap()["indexed"], 2);
    let alpha = std::fs::canonicalize(root.join("docs/alpha.md"))
        .unwrap()
        .display()
        .to_string();
    let mut request = ChatRequest::from_prompt("What is the FocalDesk build channel?");
    request.use_memory = true;
    request.memory_sources = Some(vec![alpha.clone()]);
    let response = match rpc(&socket, AiIpcRequest::Chat { request }).await {
        AiIpcResponse::Chat { response } => response,
        other => panic!("{other:?}"),
    };
    assert_eq!(response.citations.len(), 1);
    assert_eq!(
        response.citations[0].source.as_deref(),
        Some(alpha.as_str())
    );
    let supplied = requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(supplied.contains("channel is cyan"));
    assert!(!supplied.contains("channel is amber"));
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let client_socket = socket.clone();
    tokio::task::spawn_blocking(move || {
        stream_ai_chat_at(
            client_socket,
            ChatRequest::from_prompt("stream fixture"),
            |event| {
                captured.lock().unwrap().push(event);
                Ok(())
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AiStreamEvent::Delta { .. }))
    );
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(AiStreamEvent::Completed { .. })
    ));
    let secret_socket = socket.clone();
    let secret_events = tokio::task::spawn_blocking(move || {
        let mut events = Vec::new();
        stream_ai_chat_at(
            secret_socket,
            ChatRequest::from_prompt("split secret fixture"),
            |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        events
    })
    .await
    .unwrap();
    let mut published = String::new();
    for event in &secret_events {
        if let AiStreamEvent::Delta { content, .. } = event {
            published.push_str(content);
        }
    }
    assert!(published.contains("## Safe heading\n"));
    assert!(published.contains("[REDACTED]"));
    assert!(!published.contains("synthetic-secret"));
    assert!(!published.contains("person@example.com"));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut started_tx = Some(started_tx);
    let cancelled_events = Arc::new(Mutex::new(Vec::new()));
    let captured = cancelled_events.clone();
    let client_socket = socket.clone();
    let client = tokio::task::spawn_blocking(move || {
        stream_ai_chat_at(
            client_socket,
            ChatRequest::from_prompt("slow stream fixture"),
            |event| {
                if let AiStreamEvent::Started { request_id, .. } = &event {
                    if let Some(tx) = started_tx.take() {
                        let _ = tx.send(request_id.clone());
                    }
                }
                captured.lock().unwrap().push(event);
                Ok(())
            },
        )
    });
    let request_id = started_rx.await.unwrap();
    assert!(matches!(
        rpc(&socket, AiIpcRequest::CancelStream { request_id }).await,
        AiIpcResponse::Cancellation { accepted: true, .. }
    ));
    client.await.unwrap().unwrap();
    assert!(matches!(
        cancelled_events.lock().unwrap().last(),
        Some(AiStreamEvent::Cancelled { .. })
    ));
    std::fs::create_dir(root.join("cancel")).unwrap();
    for i in 0..3 {
        std::fs::write(
            root.join(format!("cancel/{i}.md")),
            format!("Cancellation fixture source {i}: completed records stay indexed."),
        )
        .unwrap();
    }
    embedder.gate.store(true, Ordering::SeqCst);
    let job = service
        .start_ingestion(IngestionSource::Directory {
            path: root.join("cancel"),
            recursive: true,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), embedder.started.notified())
        .await
        .unwrap();
    service.cancel_ingestion(&job.id).unwrap();
    embedder.release.notify_one();
    let cancelled = finished(&service, &job.id).await;
    assert_eq!(cancelled.state, "cancelled");
    assert_eq!(cancelled.result.as_ref().unwrap()["indexed"], 1);
    println!(
        "Verified folder indexing, project-only evidence, streamed deltas, generation cancellation and cooperative indexing cancellation through an isolated service."
    );
    server.abort();
    drop(service);
    drop(requests);
    let _ = std::fs::remove_dir_all(root);
}
