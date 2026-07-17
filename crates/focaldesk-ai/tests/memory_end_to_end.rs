//! Exercises `AiService::remember`/`recall` against a real local Ollama
//! instance. Requires `ollama serve` running with `nomic-embed-text` pulled
//! (`ollama pull nomic-embed-text`), so it's `#[ignore]`d by default:
//! `cargo test -p focaldesk-ai --test memory_end_to_end -- --ignored`.

use focaldesk_ai::AiService;
use focaldesk_memory::{EmbeddingProvider, MemoryService, MemoryStore, OllamaEmbeddingProvider};
use serde_json::json;
use std::sync::Arc;

fn unique_db_path() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "focaldesk-ai-memory-test-{}-{nanos}.db",
        std::process::id()
    ))
}

#[tokio::test]
#[ignore = "requires a local Ollama instance with nomic-embed-text pulled"]
async fn remember_and_recall_round_trip() {
    let db_path = unique_db_path();
    let store = MemoryStore::open(&db_path, 768).expect("open memory store");
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(
        OllamaEmbeddingProvider::new("http://127.0.0.1:11434", "nomic-embed-text", 768)
            .expect("build Ollama embedding provider"),
    );
    let memory = MemoryService::new(store, embedder);

    let service = AiService::new("ollama").with_memory(memory);
    assert!(service.has_memory());

    service
        .remember(
            "The garage door code is 4471.".into(),
            json!({ "topic": "home" }),
        )
        .await
        .expect("remember garage fact");
    service
        .remember(
            "My favorite pizza topping is mushroom.".into(),
            json!({ "topic": "food" }),
        )
        .await
        .expect("remember pizza fact");
    service
        .remember(
            "The wifi password is written on the fridge.".into(),
            json!({ "topic": "home" }),
        )
        .await
        .expect("remember wifi fact");

    let hits = service
        .recall("what's the code to open the garage".into(), 2)
        .await
        .expect("recall garage query");

    assert!(!hits.is_empty(), "expected at least one recall hit");
    assert!(
        hits[0].record.text.contains("garage door code"),
        "expected garage fact to rank first, got: {:?}",
        hits.iter().map(|hit| &hit.record.text).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_file(&db_path);
}

/// Same round trip, but through the actual Unix-socket IPC surface
/// (`focaldesk-server`'s transport) instead of calling `AiService` directly.
#[tokio::test]
#[ignore = "requires a local Ollama instance with nomic-embed-text pulled"]
async fn ipc_remember_and_recall_round_trip() {
    use focaldesk_ai::ipc::{AiIpcRequest, AiIpcResponse, send_ai_request_at, serve_ai_ipc_at};

    let db_path = unique_db_path();
    let store = MemoryStore::open(&db_path, 768).expect("open memory store");
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(
        OllamaEmbeddingProvider::new("http://127.0.0.1:11434", "nomic-embed-text", 768)
            .expect("build Ollama embedding provider"),
    );
    let memory = MemoryService::new(store, embedder);
    let service = Arc::new(AiService::new("ollama").with_memory(memory));

    let socket_path = std::env::temp_dir().join(format!(
        "focaldesk-ai-ipc-test-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&socket_path);

    let server_socket = socket_path.clone();
    tokio::spawn(async move {
        let _ = serve_ai_ipc_at(service, &server_socket).await;
    });

    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(socket_path.exists(), "AI IPC socket never came up");

    let remember_path = socket_path.clone();
    let remembered = tokio::task::spawn_blocking(move || {
        send_ai_request_at(
            &remember_path,
            &AiIpcRequest::Remember {
                text: "The office key is under the mat.".into(),
                metadata: json!({ "topic": "office" }),
            },
        )
    })
    .await
    .expect("remember task joined")
    .expect("remember IPC call succeeded");

    let id = match remembered {
        AiIpcResponse::Remembered { id } => id,
        other => panic!("expected Remembered, got {other:?}"),
    };
    assert!(id > 0);

    let recall_path = socket_path.clone();
    let recalled = tokio::task::spawn_blocking(move || {
        send_ai_request_at(
            &recall_path,
            &AiIpcRequest::Recall {
                query: "where is the office key hidden".into(),
                top_k: 3,
            },
        )
    })
    .await
    .expect("recall task joined")
    .expect("recall IPC call succeeded");

    match recalled {
        AiIpcResponse::Recalled { hits } => {
            assert!(!hits.is_empty(), "expected at least one recall hit");
            assert!(
                hits[0].record.text.contains("office key"),
                "expected office key fact to rank first, got: {:?}",
                hits.iter().map(|hit| &hit.record.text).collect::<Vec<_>>()
            );
        }
        other => panic!("expected Recalled, got {other:?}"),
    }

    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&db_path);
}
