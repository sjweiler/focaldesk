
use std::sync::Arc;
use anyhow::Result;
use tokio::sync::watch;

use crate::backend::NetworkBackend;
use crate::model::NetworkState;

pub struct NetworkService {
    backend: Arc<dyn NetworkBackend>,
    rx: watch::Receiver<NetworkState>,
}

impl NetworkService {
    pub async fn new(backend: Arc<dyn NetworkBackend>) -> Result<Self> {
        let initial = backend.current_state().await.unwrap_or_default();
        let (tx, rx) = watch::channel(initial);

        let task_backend = backend.clone();
        tokio::spawn(async move {
            let _ = task_backend.watch(tx).await;
        });

        Ok(Self { backend, rx })
    }

    pub fn subscribe(&self) -> watch::Receiver<NetworkState> {
        self.rx.clone()
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }
}
 
