use async_trait::async_trait;
use nagori_core::{AiActionId, AiOutput, AppError, Result};

use crate::{provider::AiProvider, redaction::Redactor};

#[derive(Debug, Clone, Default)]
pub struct LocalAiProvider {
    redactor: Redactor,
}

#[async_trait]
impl AiProvider for LocalAiProvider {
    async fn run_action(&self, action: AiActionId, input: &str) -> Result<AiOutput> {
        let text = match action {
            // `Redactor` only knows the built-in patterns — it can't see the
            // user's `regex_denylist`. The runtime layer is responsible for
            // routing input through the settings-aware classifier before
            // the provider runs (RedactSecrets has `require_redaction =
            // true` for exactly this reason). The pass here is a defence
            // in depth so any direct caller of the provider still gets the
            // built-in patterns scrubbed.
            AiActionId::RedactSecrets => self.redactor.redact(input),
            AiActionId::FormatJson => format_json(input)?,
            AiActionId::FormatMarkdown => input.trim().to_owned(),
            AiActionId::ExtractTasks => extract_tasks(input),
            AiActionId::Summarize => summarize(input),
            AiActionId::ExplainCode => explain_code(input),
            AiActionId::Translate | AiActionId::Rewrite => {
                return Err(AppError::Ai(
                    "this action requires a configured local or remote model".to_owned(),
                ));
            }
        };
        Ok(AiOutput {
            text,
            created_entry: None,
            warnings: Vec::new(),
        })
    }
}

fn format_json(input: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|err| AppError::Ai(err.to_string()))?;
    serde_json::to_string_pretty(&value).map_err(|err| AppError::Ai(err.to_string()))
}

fn extract_tasks(input: &str) -> String {
    input
        .lines()
        .filter(|line| {
            let lower = line.trim().to_lowercase();
            lower.contains("todo")
                || lower.contains("fixme")
                || lower.contains("task")
                || lower.starts_with("- [ ]")
        })
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize(input: &str) -> String {
    let trimmed = input.trim();
    trimmed
        .split_terminator(['.', '。', '\n'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

fn explain_code(input: &str) -> String {
    let lines = input.lines().count();
    format!("Code snippet with {lines} line(s). Local heuristic explanation only.")
}

#[cfg(test)]
mod tests {
    use nagori_core::AiActionId;

    use super::*;
    use crate::AiProvider;

    #[tokio::test]
    async fn formats_json_with_stable_pretty_output() {
        let provider = LocalAiProvider::default();

        let output = provider
            .run_action(AiActionId::FormatJson, r#"{"b":1,"a":true}"#)
            .await
            .expect("valid json should format");

        assert_eq!(output.text, "{\n  \"a\": true,\n  \"b\": 1\n}");
        assert!(output.created_entry.is_none());
        assert!(output.warnings.is_empty());
    }

    #[tokio::test]
    async fn extracts_task_like_lines_only() {
        let provider = LocalAiProvider::default();

        let output = provider
            .run_action(
                AiActionId::ExtractTasks,
                "plain line\nTODO ship it\n  - [ ] write tests\nFIXME later",
            )
            .await
            .expect("task extraction should succeed");

        assert_eq!(output.text, "TODO ship it\n- [ ] write tests\nFIXME later");
    }

    #[tokio::test]
    async fn summarizes_first_non_empty_sentence() {
        let provider = LocalAiProvider::default();

        let output = provider
            .run_action(
                AiActionId::Summarize,
                "\n\nFirst sentence. Second sentence.",
            )
            .await
            .expect("summarize should succeed");

        assert_eq!(output.text, "First sentence");
    }

    #[tokio::test]
    async fn model_required_actions_return_ai_error() {
        let provider = LocalAiProvider::default();

        let err = provider
            .run_action(AiActionId::Translate, "hello")
            .await
            .expect_err("translate requires a model");

        assert!(matches!(err, AppError::Ai(_)));
    }
}
