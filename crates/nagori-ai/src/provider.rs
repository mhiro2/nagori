use async_trait::async_trait;
use nagori_core::{AiActionId, AiOutput, Result};

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn run_action(&self, action: AiActionId, input: &str) -> Result<AiOutput>;
}

#[derive(Debug, Clone, Default)]
pub struct MockAiProvider;

#[async_trait]
impl AiProvider for MockAiProvider {
    async fn run_action(&self, action: AiActionId, input: &str) -> Result<AiOutput> {
        Ok(AiOutput {
            text: format!("{action:?}: {input}"),
            created_entry: None,
            warnings: Vec::new(),
        })
    }
}
