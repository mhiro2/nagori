use async_trait::async_trait;
use nagori_core::{AiActionId, AiOutput, Result};

use crate::provider::{AiProvider, remote_disabled};

#[derive(Debug, Clone, Default)]
pub struct RemoteAiProvider {
    pub enabled: bool,
}

#[async_trait]
impl AiProvider for RemoteAiProvider {
    async fn run_action(&self, _action: AiActionId, _input: &str) -> Result<AiOutput> {
        if !self.enabled {
            return Err(remote_disabled());
        }
        Err(remote_disabled())
    }
}
