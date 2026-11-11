use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::watch;

use crate::model::NetworkState;

pub mod networkmanager;
pub mod rtnetlink;

// async-trait adds must_use to futures that are already must_use.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait NetworkBackend: Send + Sync {
    async fn current_state(&self) -> Result<NetworkState>;
    async fn watch(&self, tx: watch::Sender<NetworkState>) -> Result<()>;
    fn name(&self) -> &'static str;
}
