//! Data types shared by the `AiActionEngine` and every transport that carries
//! its requests, events, and availability reports.
//!
//! These are plain serialisable types with no runtime or framework
//! dependencies: the engine traits and backends live in `nagori-ai`, the Apple
//! bindings in `nagori-ai-apple`, and the cancellation / streaming machinery in
//! the daemon. Keeping the wire shapes here lets the IPC layer and the desktop
//! DTO speak the same vocabulary without depending on either implementation.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::model::{AiActionId, AiInputPolicy, EntryId};

/// Identifier for one in-flight AI action.
///
/// A UUID v7 is used so identifiers sort by creation time — handy when reading
/// daemon logs or correlating a registry handle with a stream of events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub Uuid);

impl RequestId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for RequestId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Which provider family backs the AI actions.
///
/// The user selects the family; the engine's `ActionSpec` table maps
/// `(action, family)` to a concrete backend so the UI never has to know whether
/// an action runs on the language model, the translation framework, or the
/// embedding model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProviderKind {
    /// No provider — every AI action is refused.
    #[default]
    Disabled,
    /// Apple's on-device frameworks (Foundation Models / Translation /
    /// `NaturalLanguage`).
    AppleNative,
    /// A future OpenAI-compatible generic provider (not yet wired).
    OpenAiCompatible,
}

/// A request to run one AI action. Built by the daemon after redaction / size
/// shaping; the engine treats `input` as ready to hand to the backend.
// No `Eq`: `AiRequestOptions` carries an `f32` temperature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiActionRequest {
    pub request_id: RequestId,
    pub action: AiActionId,
    pub input: String,
    pub policy: AiInputPolicy,
    pub options: AiRequestOptions,
}

/// Per-request overrides — **tightening only**.
///
/// Every field can make a request *more* restrictive than the settings /
/// `ActionSpec` defaults (lower token caps, streaming off, a shorter timeout)
/// but never looser — in particular there is no `allow_remote` override, since
/// on/off-device routing is a settings-level decision, not a per-request one.
// No `Eq`: `temperature` is an `f32`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AiRequestOptions {
    pub timeout_ms: Option<u64>,
    pub max_input_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub streaming: Option<bool>,
    pub source_language: Option<String>,
    pub target_language: Option<String>,
    pub guided_schema: Option<GuidedSchema>,
    pub create_entry: bool,
    pub priority: AiPriority,
}

/// Scheduling hint for the request registry's semaphores.
///
/// The initial implementation treats every request as FIFO regardless of
/// priority; the field exists so an aging policy can be layered on without a
/// wire break.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiPriority {
    Low,
    #[default]
    Normal,
    High,
}

/// A versioned guided-generation schema.
///
/// Each variant pins an output shape so a backend can map it onto its native
/// structured-output mechanism (Apple's `@Generable`, an `OpenAI` JSON schema,
/// …) without leaking that mechanism into the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedSchema {
    /// A flat list of task strings (v1).
    ExtractTasksV1,
}

/// One event in an AI action's stream.
///
/// The terminal item of a stream is exactly one of `Ok(Done)`, `Ok(Cancelled)`,
/// or `Err(AiError)`. There is deliberately **no** `Error` event variant —
/// errors travel as the `Err` arm of the stream's `Result` so a single match
/// covers "the stream failed" and "the stream ended".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AiEvent {
    /// Text appended since the previous snapshot. `seq` is gap-free and
    /// ascending across `Delta` / `Replace`.
    Delta { seq: u64, text: String },
    /// The snapshot diverged from the previous prefix; carries the full text so
    /// the consumer replaces its buffer wholesale.
    Replace { seq: u64, text: String },
    /// Terminal: the stream completed. `final_text` is authoritative.
    Done {
        final_text: String,
        created_entry: Option<EntryId>,
        warnings: Vec<String>,
    },
    /// Terminal: the stream was cancelled before completing.
    Cancelled,
}

impl AiEvent {
    /// Whether this is a terminal event (`Done` or `Cancelled`).
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Cancelled)
    }
}

/// A structured error surfaced as the `Err` arm of the event stream (or from a
/// synchronous `start` failure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiError {
    pub code: AiErrorCode,
    pub message: String,
    /// Optional UI hint (an i18n key plus an optional CTA) so the desktop can
    /// render a "do X to fix this" affordance rather than a raw string.
    #[serde(default)]
    pub remediation: Option<Remediation>,
}

impl AiError {
    #[must_use]
    pub fn new(code: AiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remediation: None,
        }
    }

    #[must_use]
    pub fn with_remediation(mut self, remediation: Remediation) -> Self {
        self.remediation = Some(remediation);
        self
    }
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for AiError {}

/// Coarse error classes the UI and CLI can branch on without parsing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiErrorCode {
    /// The provider/backend is not available (e.g. Apple Intelligence off).
    Unavailable,
    /// The resolver found no backend for `(action, provider)`.
    CapabilityMismatch,
    /// The input exceeds the model's token budget (refused, not truncated).
    InputTooLarge,
    /// The request exceeded its timeout.
    Timeout,
    /// A backend / FFI-layer failure (Swift bridge, Apple API).
    BackendInternal,
    /// A required asset (translation pack, embedding model) is missing.
    AssetMissing,
    /// The backend is rate limited (background asset generation, etc.).
    RateLimited,
    /// An unclassified failure.
    Unknown,
}

/// A UI remediation hint attached to an [`AiError`] or availability entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    /// i18n key the UI resolves to a localized hint + call to action.
    pub i18n_key: String,
    /// Optional concrete action the UI can wire to a button.
    #[serde(default)]
    pub action: Option<RemediationAction>,
}

impl Remediation {
    #[must_use]
    pub fn new(i18n_key: impl Into<String>) -> Self {
        Self {
            i18n_key: i18n_key.into(),
            action: None,
        }
    }

    #[must_use]
    pub const fn with_action(mut self, action: RemediationAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// A concrete remediation the UI can offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationAction {
    /// Deep-link to the Apple Intelligence pane in System Settings.
    OpenAppleIntelligenceSettings,
    /// Offer to switch to a different provider family.
    SwitchProvider,
    /// Offer to retry once an asset has downloaded.
    Retry,
}

/// A technical capability a backend declares. Action-specific availability is
/// resolved separately via the `ActionSpec` table; these describe *what a
/// backend can do*, not *which actions are enabled*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCapability {
    TextGeneration,
    StreamingText,
    GuidedGeneration,
    Translation,
    EmbeddingBatch,
    OnDevice,
    RequiresAssets,
    LanguagePairMatrix,
}

/// A set of [`AiCapability`] flags a backend or engine exposes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiCapabilitySet(pub std::collections::BTreeSet<AiCapability>);

impl AiCapabilitySet {
    #[must_use]
    pub fn contains(&self, capability: AiCapability) -> bool {
        self.0.contains(&capability)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<AiCapability> for AiCapabilitySet {
    fn from_iter<T: IntoIterator<Item = AiCapability>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// A point-in-time availability report. The desktop caches this with a short
/// TTL rather than polling continuously; `nagori doctor` renders it verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAvailabilityReport {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub provider: AiProviderKind,
    pub overall_status: AiOverallStatus,
    /// Per-action availability, in capability-matrix order.
    pub per_action: Vec<PerActionAvailability>,
    /// Status of the (separately gated) semantic index.
    pub semantic_index: SemanticIndexAvailability,
}

/// The headline availability state across all AI actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOverallStatus {
    /// At least one action is runnable.
    Available,
    /// AI is enabled but no action is currently runnable (e.g. OS unavailable).
    Unavailable,
    /// The master AI toggle is off.
    Disabled,
}

/// Availability of one AI action, with an optional remediation hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerActionAvailability {
    pub action: AiActionId,
    pub status: PerActionStatus,
    #[serde(default)]
    pub remediation: Option<Remediation>,
}

/// Why a given action is (un)available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerActionStatus {
    /// Runnable now.
    Available,
    /// Disabled by the settings allow-list / master toggle.
    DisabledBySettings,
    /// No backend resolves for `(action, provider)`.
    CapabilityMismatch,
    /// The OS reports the underlying model/framework unavailable.
    OsUnavailable,
    /// A required asset (language pack / embedding model) is missing.
    AssetMissing,
    /// The current language is unsupported for this action.
    LanguageUnsupported,
    /// No provider is configured.
    NotConfigured,
    /// Indeterminate.
    Unknown,
}

/// Availability of the semantic index feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIndexAvailability {
    /// Disabled in settings.
    Disabled,
    /// Enabled in settings but not yet implemented in this build.
    NotImplemented,
}

/// A conservative estimate of how many tokens `input` will consume.
///
/// Apple's Foundation Models cap a session at 4,096 tokens (instructions +
/// prompt + output) and silently truncate on overflow, so the daemon refuses
/// oversized input *before* dispatch rather than letting the model drop text.
/// CJK scripts run ~1 token per character; Latin text runs closer to ~4
/// characters per token. We count CJK / wide scalars as a full token each and
/// the rest at a quarter-token, rounding up — biased toward over-counting so
/// the guard errs on the side of rejecting borderline input.
#[must_use]
pub fn estimate_tokens(input: &str) -> usize {
    let mut weighted = 0usize;
    for ch in input.chars() {
        // Scale by 4 so a non-CJK character contributes 1 (≈ 0.25 token) and a
        // CJK / wide character contributes 4 (= 1 token); divide at the end.
        weighted += if is_cjk_or_wide(ch) { 4 } else { 1 };
    }
    // Round up so any non-empty input is at least one token.
    weighted.div_ceil(4)
}

/// Whether a scalar belongs to a script Apple bills at ~1 token per character.
const fn is_cjk_or_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK radicals / Kangxi / CJK symbols & punctuation
        | 0x3041..=0x33FF      // Hiragana, Katakana, CJK symbols, enclosed CJK
        | 0x3400..=0x4DBF      // CJK Extension A
        | 0x4E00..=0x9FFF      // CJK Unified Ideographs
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFF00..=0xFF60      // Fullwidth forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
        | 0x1F300..=0x1FAFF    // emoji / symbols & pictographs
        | 0x20000..=0x3FFFD    // CJK Extension B+ (supplementary ideographic plane)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_round_trips_through_string() {
        let id = RequestId::new();
        let parsed: RequestId = id.to_string().parse().expect("round-trip");
        assert_eq!(id, parsed);
    }

    #[test]
    fn request_ids_are_time_ordered() {
        let a = RequestId::new();
        let b = RequestId::new();
        // UUID v7 embeds a millisecond timestamp; two ids minted back to back
        // never decrease.
        assert!(a.0 <= b.0);
    }

    #[test]
    fn ai_event_terminal_classification() {
        assert!(
            AiEvent::Done {
                final_text: "x".to_owned(),
                created_entry: None,
                warnings: Vec::new(),
            }
            .is_terminal()
        );
        assert!(AiEvent::Cancelled.is_terminal());
        assert!(
            !AiEvent::Delta {
                seq: 0,
                text: "x".to_owned()
            }
            .is_terminal()
        );
    }

    #[test]
    fn ai_event_serializes_with_type_tag() {
        let json = serde_json::to_string(&AiEvent::Delta {
            seq: 3,
            text: "hi".to_owned(),
        })
        .expect("serialize");
        assert!(json.contains("\"type\":\"delta\""));
        assert!(json.contains("\"seq\":3"));
    }

    #[test]
    fn cjk_is_billed_at_one_token_per_char() {
        // Five kana ≈ five tokens.
        assert_eq!(estimate_tokens("こんにちは"), 5);
    }

    #[test]
    fn latin_text_is_roughly_quarter_token_per_char() {
        // 8 ASCII chars → ceil(8/4) = 2 tokens.
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn empty_input_is_zero_tokens() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn capability_set_membership() {
        let set: AiCapabilitySet = [AiCapability::TextGeneration, AiCapability::StreamingText]
            .into_iter()
            .collect();
        assert!(set.contains(AiCapability::TextGeneration));
        assert!(!set.contains(AiCapability::Translation));
    }
}
