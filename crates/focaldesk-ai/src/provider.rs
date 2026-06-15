use anyhow::Result;
use async_trait::async_trait;

use crate::types::{ChatRequest, ChatResponse, ProviderInfo};

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
}
