use serde::{Deserialize, Serialize};

use super::EntryId;

mod engine;

pub use engine::{
    AiActionRequest, AiAvailabilityReport, AiCapability, AiCapabilitySet, AiError, AiErrorCode,
    AiEvent, AiOverallStatus, AiPriority, AiProviderKind, AiRequestOptions, GuidedSchema,
    PerActionAvailability, PerActionStatus, Remediation, RemediationAction, RequestId,
    SemanticIndexAvailability, char_token_quarters, estimate_tokens,
};

/// A rule-based "Quick action": always available on-device.
///
/// Quick actions never touch a language model regardless of the AI provider
/// configuration — they are deterministic transforms surfaced from the
/// palette's action menu.
///
/// This is deliberately a *separate* enum from [`AiActionId`]: the two used to
/// be conflated behind an `is_quick_action()` predicate, which forced every
/// dispatch site to branch on the same variant by call context. Splitting them
/// lets the type system distinguish "deterministic on-device transform" from
/// "model-backed AI action" with no runtime predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum QuickActionId {
    /// Pretty-print JSON input.
    FormatJson,
    /// Extract `TODO` / `FIXME` / checkbox lines heuristically.
    ExtractTasks,
    /// Redact secrets using the settings-aware classifier.
    RedactSecrets,
    /// Return the first non-empty sentence (the former rule-based `Summarize`).
    SummarizeFirstSentence,
}

impl QuickActionId {
    /// Stable kebab-case identifier, used by the CLI and logs.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::FormatJson => "format-json",
            Self::ExtractTasks => "extract-tasks",
            Self::RedactSecrets => "redact-secrets",
            Self::SummarizeFirstSentence => "summarize-first-sentence",
        }
    }

    /// The input policy that gates this quick action.
    #[must_use]
    pub const fn input_policy(self) -> AiInputPolicy {
        let require_redaction = match self {
            // `RedactSecrets` is the redaction action, but it still routes
            // through the settings-aware classifier so the user's
            // `regex_denylist` applies even on Public entries. The summarise
            // / extract heuristics see the raw text only after redaction.
            Self::RedactSecrets | Self::SummarizeFirstSentence | Self::ExtractTasks => true,
            Self::FormatJson => false,
        };
        AiInputPolicy {
            allow_remote: false,
            require_redaction,
            max_bytes: 64 * 1024,
        }
    }
}

/// A model-backed AI action, resolved through the `AiActionEngine`.
///
/// Resolves to a concrete backend (Apple on-device today; an
/// `OpenAI`-compatible provider later). Distinct from [`QuickActionId`]: these
/// require an available provider and stream their output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum AiActionId {
    /// Summarise the input via a language model.
    Summarize,
    /// Translate the input via the platform translation framework.
    Translate,
    /// Rewrite / rephrase the input.
    Rewrite,
    /// Reformat the input as Markdown.
    FormatMarkdown,
    /// Extract structured tasks via guided generation (distinct from the
    /// heuristic [`QuickActionId::ExtractTasks`]).
    ExtractTasks,
    /// Explain a code snippet (Apple docs flag code reasoning as out of scope,
    /// so this stays last and quality-gated).
    ExplainCode,
}

impl AiActionId {
    /// Stable kebab-case identifier, used by the CLI and logs.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Summarize => "summarize",
            Self::Translate => "translate",
            Self::Rewrite => "rewrite",
            Self::FormatMarkdown => "format-markdown",
            Self::ExtractTasks => "extract-tasks",
            Self::ExplainCode => "explain-code",
        }
    }

    /// Every AI action, in capability-matrix order. Used to build availability
    /// reports and to validate the settings allow-list.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Summarize,
            Self::Translate,
            Self::Rewrite,
            Self::FormatMarkdown,
            Self::ExtractTasks,
            Self::ExplainCode,
        ]
    }

    /// The input policy that gates this AI action. Apple's on-device models run
    /// fully local, so `allow_remote` is always `false`; the byte cap is a
    /// coarse guard, with the precise token-budget check applied separately
    /// (see [`engine::estimate_tokens`]).
    ///
    /// Every AI action requires redaction: the model sees the input text and
    /// these actions preserve wording (so a denylisted token could be echoed
    /// straight back), so the settings-aware classifier must shape the input
    /// for all of them — including `FormatMarkdown`, which only reformats.
    #[must_use]
    pub const fn input_policy(self) -> AiInputPolicy {
        AiInputPolicy {
            allow_remote: false,
            require_redaction: true,
            max_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiInputPolicy {
    pub allow_remote: bool,
    pub require_redaction: bool,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOutputPolicy {
    pub may_create_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOutput {
    pub text: String,
    pub created_entry: Option<EntryId>,
    pub warnings: Vec<String>,
}
