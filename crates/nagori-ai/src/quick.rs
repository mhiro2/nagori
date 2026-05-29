//! The rule-based "Quick action" runner.
//!
//! Quick actions are deterministic on-device transforms — no language model is
//! involved — so they run synchronously and are always available regardless of
//! the AI provider configuration. The daemon applies the settings-aware
//! redaction classifier before calling here; the [`Redactor`] pass inside
//! [`QuickActionRunner::run`] is a defence-in-depth scrub of the built-in
//! secret patterns for any caller that bypasses the classifier.

use nagori_core::{AiOutput, AppError, QuickActionId, Result};

use crate::redaction::Redactor;

/// Runs the deterministic quick actions.
#[derive(Debug, Clone, Default)]
pub struct QuickActionRunner {
    redactor: Redactor,
}

impl QuickActionRunner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `action` against `input`, returning the transformed text.
    pub fn run(&self, action: QuickActionId, input: &str) -> Result<AiOutput> {
        let text = match action {
            // The classifier upstream already redacts with the user's rules;
            // this built-in pass is a belt-and-braces scrub for direct callers.
            QuickActionId::RedactSecrets => self.redactor.redact(input),
            QuickActionId::FormatJson => format_json(input)?,
            QuickActionId::ExtractTasks => extract_tasks(input),
            QuickActionId::SummarizeFirstSentence => summarize_first_sentence(input),
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

fn summarize_first_sentence(input: &str) -> String {
    let trimmed = input.trim();
    trimmed
        .split_terminator(['.', '。', '\n'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_json_with_stable_pretty_output() {
        let runner = QuickActionRunner::new();
        let output = runner
            .run(QuickActionId::FormatJson, r#"{"b":1,"a":true}"#)
            .expect("valid json should format");
        assert_eq!(output.text, "{\n  \"a\": true,\n  \"b\": 1\n}");
        assert!(output.created_entry.is_none());
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn extracts_task_like_lines_only() {
        let runner = QuickActionRunner::new();
        let output = runner
            .run(
                QuickActionId::ExtractTasks,
                "plain line\nTODO ship it\n  - [ ] write tests\nFIXME later",
            )
            .expect("task extraction should succeed");
        assert_eq!(output.text, "TODO ship it\n- [ ] write tests\nFIXME later");
    }

    #[test]
    fn summarizes_first_non_empty_sentence() {
        let runner = QuickActionRunner::new();
        let output = runner
            .run(
                QuickActionId::SummarizeFirstSentence,
                "\n\nFirst sentence. Second sentence.",
            )
            .expect("summarize should succeed");
        assert_eq!(output.text, "First sentence");
    }

    #[test]
    fn invalid_json_returns_ai_error() {
        let runner = QuickActionRunner::new();
        let err = runner
            .run(QuickActionId::FormatJson, "not json")
            .expect_err("invalid json should error");
        assert!(matches!(err, AppError::Ai(_)));
    }
}
