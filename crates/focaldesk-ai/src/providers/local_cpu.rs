use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::provider::AiProvider;
use crate::types::{ChatRequest, ChatResponse, ProviderInfo};

#[derive(Debug, Default, Clone)]
pub struct LocalCpuProvider;

#[async_trait]
impl AiProvider for LocalCpuProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "local-cpu".into(),
            kind: "local-cpu".into(),
            base_url: None,
            default_model: None,
        }
    }

    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
        bail!("local CPU inference is not implemented yet")
    }
}
