use anyhow::Result;
use async_trait::async_trait;

use crate::types::{ChatRequest, ChatResponse, ProviderInfo, ProviderModelInfo};

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>>;

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
}
