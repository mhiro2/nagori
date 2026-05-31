use nagori_core::{
    AiActionId, AiAvailabilityReport, AiOutput, AiOverallStatus, ClipboardEntry, ContentKind,
    EntryId, PerActionStatus, RankReason, RepresentationRole, RepresentationSummary, SearchFilters,
    SearchMode, SearchResult, SemanticIndexState, SemanticIndexStatus, Sensitivity,
    safe_preview_for_dto,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

mod platform;
mod preview;
mod settings;
pub use platform::*;
pub use preview::*;
pub use settings::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentKindDto {
    Text,
    Url,
    Code,
    Image,
    FileList,
    RichText,
    Unknown,
}

impl From<ContentKind> for ContentKindDto {
    fn from(kind: ContentKind) -> Self {
        match kind {
            ContentKind::Text => Self::Text,
            ContentKind::Url => Self::Url,
            ContentKind::Code => Self::Code,
            ContentKind::Image => Self::Image,
            ContentKind::FileList => Self::FileList,
            ContentKind::RichText => Self::RichText,
            ContentKind::Unknown => Self::Unknown,
        }
    }
}

impl From<ContentKindDto> for ContentKind {
    fn from(kind: ContentKindDto) -> Self {
        match kind {
            ContentKindDto::Text => Self::Text,
            ContentKindDto::Url => Self::Url,
            ContentKindDto::Code => Self::Code,
            ContentKindDto::Image => Self::Image,
            ContentKindDto::FileList => Self::FileList,
            ContentKindDto::RichText => Self::RichText,
            ContentKindDto::Unknown => Self::Unknown,
        }
    }
}

pub(crate) fn default_capture_kind_dtos() -> Vec<ContentKindDto> {
    nagori_core::settings::default_capture_kinds()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepresentationRoleDto {
    Primary,
    PlainFallback,
    Alternative,
}

impl From<RepresentationRole> for RepresentationRoleDto {
    fn from(role: RepresentationRole) -> Self {
        match role {
            RepresentationRole::Primary => Self::Primary,
            RepresentationRole::PlainFallback => Self::PlainFallback,
            RepresentationRole::Alternative => Self::Alternative,
        }
    }
}

/// Wire-safe projection of one preserved representation row. Mirrors
/// `nagori_ipc::RepresentationSummaryDto` but serialises in camelCase so
/// the Svelte side can consume the field without a transformation layer.
/// Bytes/text stay daemon-side; only the MIME type, role, and byte count
/// reach the renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationSummaryDto {
    pub mime_type: String,
    pub role: RepresentationRoleDto,
    pub byte_count: u64,
}

impl RepresentationSummaryDto {
    pub fn from_summary(summary: &RepresentationSummary) -> Self {
        Self {
            mime_type: summary.mime_type.clone(),
            role: summary.role.into(),
            byte_count: summary.byte_count,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub sensitivity: Sensitivity,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl EntryDto {
    pub fn from_entry(entry: ClipboardEntry, include_text: bool) -> Self {
        let preview = safe_preview_for_dto(&entry);
        Self {
            id: entry.id,
            kind: entry.content_kind().into(),
            text: include_text.then(|| entry.plain_text().unwrap_or_default().to_owned()),
            preview,
            created_at: entry.metadata.created_at,
            updated_at: entry.metadata.updated_at,
            last_used_at: entry.metadata.last_used_at,
            use_count: entry.metadata.use_count,
            pinned: entry.lifecycle.pinned,
            source_app_name: entry.metadata.source.and_then(|source| source.name),
            sensitivity: entry.sensitivity,
            representation_summary: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    pub preview: String,
    pub score: f32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub pinned: bool,
    pub sensitivity: Sensitivity,
    pub rank_reasons: Vec<RankReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl From<SearchResult> for SearchResultDto {
    fn from(value: SearchResult) -> Self {
        Self {
            id: value.entry_id,
            kind: value.content_kind.into(),
            preview: value.preview,
            score: value.score,
            created_at: value.created_at,
            pinned: value.pinned,
            sensitivity: value.sensitivity,
            rank_reasons: value.rank_reason,
            source_app_name: value.source_app_name,
            representation_summary: Vec::new(),
        }
    }
}

impl SearchResultDto {
    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFiltersDto {
    #[serde(default)]
    pub kinds: Vec<ContentKindDto>,
    #[serde(default)]
    pub pinned_only: bool,
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_after: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_before: Option<OffsetDateTime>,
}

impl From<SearchFiltersDto> for SearchFilters {
    fn from(value: SearchFiltersDto) -> Self {
        Self {
            kinds: value.kinds.into_iter().map(Into::into).collect(),
            pinned_only: value.pinned_only,
            source_app: value.source_app,
            created_after: value.created_after,
            created_before: value.created_before,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequestDto {
    pub query: String,
    #[serde(default)]
    pub mode: Option<SearchMode>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub filters: Option<SearchFiltersDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseDto {
    pub results: Vec<SearchResultDto>,
    pub total_candidates: usize,
    /// Time spent in the search pipeline itself (`NagoriRuntime::search`).
    pub search_elapsed_ms: u64,
    /// Time spent hydrating representation summaries for the result set.
    pub summary_elapsed_ms: u64,
    /// End-to-end time the command spent producing the response — the value
    /// the UI surfaces. `total - (search + summary)` is the DTO-shaping
    /// overhead, kept implicit rather than as a fourth field.
    pub total_elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiActionResultDto {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_entry_id: Option<EntryId>,
    pub warnings: Vec<String>,
}

impl From<AiOutput> for AiActionResultDto {
    fn from(value: AiOutput) -> Self {
        Self {
            text: value.text,
            created_entry_id: value.created_entry,
            warnings: value.warnings,
        }
    }
}

/// Per-action availability, flattened for the renderer's gating logic.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiActionAvailabilityDto {
    pub action: AiActionId,
    pub status: PerActionStatus,
    /// `true` only when the action can run right now.
    pub available: bool,
    /// i18n key for a UI remediation hint (e.g. "open System Settings"), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Wire shape of [`AiAvailabilityReport`] for the desktop. Surfaces the overall
/// status plus per-action availability so the palette can disable unavailable
/// AI actions and show a reason.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAvailabilityDto {
    pub provider: AiProviderKindDto,
    pub overall_status: AiOverallStatus,
    pub actions: Vec<AiActionAvailabilityDto>,
}

impl From<AiAvailabilityReport> for AiAvailabilityDto {
    fn from(value: AiAvailabilityReport) -> Self {
        Self {
            provider: value.provider.into(),
            overall_status: value.overall_status,
            actions: value
                .per_action
                .into_iter()
                .map(|entry| AiActionAvailabilityDto {
                    available: entry.status == PerActionStatus::Available,
                    action: entry.action,
                    status: entry.status,
                    remediation: entry.remediation.map(|rem| rem.i18n_key),
                })
                .collect(),
        }
    }
}

/// Wire shape of [`SemanticIndexStatus`] for the settings UI: the coarse state
/// plus indexed / pending / total counts and the model identifier.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticIndexStatusDto {
    pub state: SemanticIndexState,
    pub indexed: u64,
    pub pending: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl From<SemanticIndexStatus> for SemanticIndexStatusDto {
    fn from(value: SemanticIndexStatus) -> Self {
        Self {
            state: value.state,
            indexed: value.indexed,
            pending: value.pending,
            total: value.total,
            model: value.model.map(|meta| meta.model_identifier),
        }
    }
}

/// Wire-shape mirror of `state::HotkeyFailureRecord`. Returned by the
/// `last_hotkey_failure` command so the always-on App-level subscriber
/// can re-hydrate the toast/banner if the live event fired before its
/// listener attached. The field shape matches the
/// `nagori://hotkey_register_failed` emit envelope so the frontend
/// store can share a single normaliser between the two paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyFailureDto {
    pub hotkey: String,
    pub error: String,
    /// `Some("secondary")` for secondary accelerators; absent for the
    /// primary palette shortcut — mirrors `build_hotkey_failure_payload`
    /// in `lib.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Kebab-case wire value of the secondary action whose register
    /// failed (`repaste-last`, `clear-history`). Absent for primary
    /// failures. The frontend store reads this so a later resolved
    /// event targeting a *different* secondary action can be ignored
    /// instead of wiping the displayed banner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl From<crate::state::HotkeyFailureRecord> for HotkeyFailureDto {
    fn from(value: crate::state::HotkeyFailureRecord) -> Self {
        Self {
            hotkey: value.hotkey,
            error: value.error,
            kind: value.kind,
            action: value.action,
        }
    }
}

/// Current state of the bundled `nagori` CLI relative to the user's `PATH`.
/// Surfaced read-only in Settings → CLI so the "Install" button can render
/// the right affordance (install / re-link / already linked) without the
/// renderer probing the filesystem itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallStatusDto {
    /// Whether this OS build supports the one-click install at all. macOS and
    /// Linux symlink into `~/.local/bin`; Windows is `false` for now and the
    /// UI shows manual guidance instead.
    pub supported: bool,
    /// Whether the CLI binary actually shipped beside the desktop executable
    /// (false under `tauri dev`, where sidecars are not copied next to the
    /// dev binary).
    pub bundled: bool,
    /// Whether `<bin_dir>/nagori` already resolves to the bundled binary.
    pub installed: bool,
    /// Symlink destination this build would create / has created.
    pub installed_path: String,
    /// Directory the symlink lives in (`~/.local/bin`).
    pub bin_dir: String,
    /// Best-effort: whether `bin_dir` is on the user's shell `PATH`.
    pub on_path: bool,
}

/// Result of a successful `install_cli` call. Mirrors the status shape minus
/// the capability flags so the UI can confirm where the link landed and
/// whether the user still needs to extend their `PATH`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallResultDto {
    /// Symlink that now points at the bundled binary.
    pub installed_path: String,
    /// Directory the symlink was created in (`~/.local/bin`).
    pub bin_dir: String,
    /// Bundled binary the symlink resolves to.
    pub source_path: String,
    /// Best-effort: whether `bin_dir` is on the user's shell `PATH`.
    pub on_path: bool,
}

#[cfg(test)]
mod tests {
    use nagori_core::{EntryFactory, Sensitivity};

    use super::*;

    fn text_entry(body: &str) -> nagori_core::ClipboardEntry {
        EntryFactory::from_text(body)
    }

    #[test]
    fn entry_dto_omits_text_for_private_or_secret_unless_caller_opts_in() {
        // `EntryDto::from_entry` exposes the `include_text` flag so the
        // command layer can keep raw bodies out of the default response shape
        // for sensitive entries while still returning text on copy/paste paths.
        let mut entry = text_entry("super secret value");
        entry.sensitivity = Sensitivity::Secret;

        let stripped = EntryDto::from_entry(entry.clone(), false);
        assert!(stripped.text.is_none());
        let with_text = EntryDto::from_entry(entry, true);
        assert_eq!(with_text.text.as_deref(), Some("super secret value"));
    }
}
