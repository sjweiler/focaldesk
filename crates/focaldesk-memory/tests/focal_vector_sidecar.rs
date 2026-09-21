use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use focaldesk_memory::{MemoryPolicy, MemoryStore};
use serde_json::json;

struct Sidecar(Child);

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires FOCAL_VECTOR_TEST_BIN pointing to focal-server"]
fn focal_vector_backend_round_trip() {
    let binary = std::env::var("FOCAL_VECTOR_TEST_BIN").expect("FOCAL_VECTOR_TEST_BIN is required");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "focaldesk-focal-vector-test-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("focal-vector.sock");
    let data = root.join("vectors");
    let database = root.join("memory.db");

    let child = Command::new(binary)
        .env("FOCAL_VECTOR_SOCKET", &socket)
        .env("FOCAL_DATA_DIR", &data)
        .env("RAYON_NUM_THREADS", "2")
        .env("FOCAL_MAX_CONCURRENT_OPERATIONS", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start focal-server");
    let _sidecar = Sidecar(child);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "Focal Vector socket was not created"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { std::env::set_var("FOCAL_VECTOR_SOCKET", &socket) };

    let store = MemoryStore::open_focal_vector_with_policy(
        &database,
        4,
        MemoryPolicy {
            retention: None,
            max_entries: Some(100),
        },
        Some("integration-memories".into()),
    )
    .expect("open Focal Vector memory store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let first = store
            .remember(
                "the garage code is 2468".into(),
                vec![1.0, 0.0, 0.0, 0.0],
                json!({"source": "test"}),
            )
            .await
            .unwrap();
        store
            .remember(
                "buy oat milk".into(),
                vec![0.0, 1.0, 0.0, 0.0],
                json!({"source": "test"}),
            )
            .await
            .unwrap();

        let hits = store.recall(vec![0.99, 0.01, 0.0, 0.0], 2).await.unwrap();
        assert_eq!(hits[0].record.id, first);
        assert_eq!(hits[0].record.text, "the garage code is 2468");
        assert_eq!(store.status().await.unwrap().vector_backend, "focal-vector");

        store.forget(first).await.unwrap();
        let hits = store.recall(vec![1.0, 0.0, 0.0, 0.0], 2).await.unwrap();
        assert!(hits.iter().all(|hit| hit.record.id != first));
        assert_eq!(store.clear().await.unwrap(), 1);
    });

    drop(store);
    unsafe { std::env::remove_var("FOCAL_VECTOR_SOCKET") };
    let _ = std::fs::remove_dir_all(root);
}
