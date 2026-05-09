use async_trait::async_trait;
use nagori_core::{AiActionId, AiOutput, AppError, Result};

use crate::provider::AiProvider;

/// Placeholder for an OpenAI-backed remote provider.
///
/// This is **not** a working implementation. The real provider
/// (request signing, model-name routing, streaming, retry/backoff,
/// telemetry-redaction) lands behind a future `openai` Cargo feature
/// and a vetted dependency on `reqwest`. Until then, every call returns
/// `AppError::Ai` with a message that makes the unimplemented status
/// unambiguous so a user who configured `ai_provider = "remote"`
/// doesn't see a generic "AI failed" string and assume the request
/// silently dropped.
///
/// The struct is intentionally named with a `Stub` prefix so a future
/// real implementation can land alongside it (`OpenAiProvider`) with no
/// possibility of mistaking the two at a `use` site.
#[derive(Debug, Clone, Default)]
pub struct StubOpenAiProvider {
    /// Mirrors the historical config knob: `true` if the user opted in
    /// to remote AI. Surfaced in the error so the message can call out
    /// "you asked for remote AI but this build doesn't ship one"
    /// distinctly from "remote AI is disabled by your settings".
    pub enabled: bool,
}

#[async_trait]
impl AiProvider for StubOpenAiProvider {
    async fn run_action(&self, _action: AiActionId, _input: &str) -> Result<AiOutput> {
        let detail = if self.enabled {
            "OpenAI remote AI provider is not implemented in this build. \
             Switch `ai_provider` to `local` or `none` until the OpenAI \
             integration ships."
        } else {
            "OpenAI remote AI provider is not implemented in this build, \
             and `ai_provider` is currently disabled. Set it to `local` \
             or `none` to use AI actions today."
        };
        Err(AppError::Ai(detail.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enabled_stub_reports_unimplemented_status_to_user() {
        let provider = StubOpenAiProvider { enabled: true };
        let err = provider
            .run_action(AiActionId::Summarize, "hello")
            .await
            .expect_err("stub must always error");
        let message = err.to_string();
        assert!(
            message.contains("not implemented"),
            "stub error must announce unimplemented status, got: {message}",
        );
    }

    #[tokio::test]
    async fn disabled_stub_distinguishes_unimplemented_from_disabled() {
        let provider = StubOpenAiProvider { enabled: false };
        let err = provider
            .run_action(AiActionId::Summarize, "hello")
            .await
            .expect_err("stub must always error");
        let message = err.to_string();
        assert!(
            message.contains("not implemented") && message.contains("disabled"),
            "disabled stub must mention both unimplemented and disabled, got: {message}",
        );
    }
}
