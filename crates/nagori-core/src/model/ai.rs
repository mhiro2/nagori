use serde::{Deserialize, Serialize};

use super::EntryId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAction {
    pub id: AiActionId,
    pub name: String,
    pub input_policy: AiInputPolicy,
    pub output_policy: AiOutputPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum AiActionId {
    Summarize,
    Translate,
    FormatJson,
    FormatMarkdown,
    ExplainCode,
    Rewrite,
    ExtractTasks,
    RedactSecrets,
}

impl AiActionId {
    /// Quick actions are the four entries surfaced by the desktop
    /// palette's action menu; they always run against the on-device
    /// rule-based runner regardless of `ai_enabled` / `ai_provider`.
    /// Legacy variants (`Translate`, `Rewrite`, `FormatMarkdown`,
    /// `ExplainCode`) remain in the enum for schema compatibility but
    /// have no UI entry point — they still flow through the regular
    /// provider gating logic.
    #[must_use]
    pub const fn is_quick_action(&self) -> bool {
        matches!(
            self,
            Self::Summarize | Self::FormatJson | Self::ExtractTasks | Self::RedactSecrets
        )
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
