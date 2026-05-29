//! Lower-tier backend traits the [`crate::AiActionEngine`] dispatches to.
//!
//! Each backend owns one capability family — text generation, translation, or
//! embedding — and is deliberately ignorant of `AiActionId`-level routing,
//! which the engine resolves through its `ActionSpec` table. Keeping the
//! Swift-specific machinery (Apple's `Generable`, `TranslationSession`,
//! `NLContextualEmbedding`) behind these traits means `nagori-ai` stays free of
//! any Apple dependency: the `nagori-ai-apple` crate implements the traits and
//! the daemon injects the implementations.

use async_trait::async_trait;
use nagori_core::{
    AiActionId, AiError, AiErrorCode, AiRequestOptions, GuidedSchema, Remediation,
    RemediationAction, RequestId,
};
use tokio_util::sync::CancellationToken;

use crate::AiEventStream;

/// A backend request to generate text for one resolved action.
///
/// The input is already redaction-shaped and size-checked by the daemon; the
/// backend only has to turn it into a prompt for its model.
#[derive(Debug, Clone)]
pub struct TextGenerationRequest {
    pub request_id: RequestId,
    pub action: AiActionId,
    pub input: String,
    pub options: AiRequestOptions,
    /// Optional structured-output schema for guided generation.
    pub guided_schema: Option<GuidedSchema>,
}

/// What a [`TextGenerator`] can do. Mirrors the engine-level capability flags
/// but scoped to a single backend so the resolver can reason about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextGenerationCapabilities {
    pub streaming: bool,
    pub guided_generation: bool,
    pub on_device: bool,
}

/// Translation request (one source/target pair).
#[derive(Debug, Clone)]
pub struct TranslationRequest {
    pub request_id: RequestId,
    pub input: String,
    pub source_language: Option<String>,
    pub target_language: String,
}

/// Result of a (non-streaming) translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationOutput {
    pub text: String,
    pub detected_source_language: Option<String>,
}

/// One input to embed, tagged so the caller can correlate results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingInput {
    pub id: String,
    pub text: String,
}

/// A single embedding vector plus the id it was produced for.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingVector {
    pub id: String,
    pub vector: Vec<f32>,
}

/// Runtime-read identity of an embedding model.
///
/// Every field is read from the model at runtime (never baked) so the index
/// can detect a model / revision / dimension change and rebuild rather than
/// mixing incompatible embedding spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModelMetadata {
    /// Opaque model identifier (Apple's `NLContextualEmbedding.modelIdentifier`).
    pub model_identifier: String,
    /// Model revision; a bump changes the embedding space.
    pub revision: u32,
    /// Vector dimensionality.
    pub dimension: usize,
    /// Token cap above which the model silently truncates; the indexer chunks
    /// longer inputs instead of dropping the tail.
    pub max_sequence_length: usize,
    /// Locale identifiers the model covers.
    pub languages: Vec<String>,
}

/// OS-level availability of a backend, independent of settings gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendAvailability {
    /// The backend can run now.
    Available,
    /// The backend cannot run; `reason` explains why.
    Unavailable(BackendUnavailableReason),
}

impl BackendAvailability {
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Why a backend is unavailable. Maps onto the OS-reported reasons (Apple
/// Intelligence states, asset download state) without leaking the Apple enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendUnavailableReason {
    /// The device is not eligible (pre-M1 silicon, non-Apple host).
    DeviceNotEligible,
    /// Apple Intelligence is not enabled in System Settings.
    NotEnabled,
    /// The model is still downloading or otherwise not ready.
    ModelNotReady,
    /// A required asset (language pack, embedding model) is missing.
    AssetMissing,
    /// Background asset generation is rate limited.
    RateLimited,
    /// An unrecognised unavailable state.
    Unknown,
}

impl BackendUnavailableReason {
    /// The engine-level error code this reason maps to when a `start` is
    /// attempted against an unavailable backend.
    #[must_use]
    pub const fn error_code(self) -> AiErrorCode {
        match self {
            Self::AssetMissing => AiErrorCode::AssetMissing,
            Self::RateLimited => AiErrorCode::RateLimited,
            _ => AiErrorCode::Unavailable,
        }
    }

    /// A UI remediation hint for this reason, if one applies.
    #[must_use]
    pub fn remediation(self) -> Option<Remediation> {
        match self {
            Self::NotEnabled => Some(
                Remediation::new("ai.unavailable.apple_intelligence_not_enabled")
                    .with_action(RemediationAction::OpenAppleIntelligenceSettings),
            ),
            Self::DeviceNotEligible => Some(
                Remediation::new("ai.unavailable.device_not_eligible")
                    .with_action(RemediationAction::SwitchProvider),
            ),
            Self::ModelNotReady => Some(
                Remediation::new("ai.unavailable.model_not_ready")
                    .with_action(RemediationAction::Retry),
            ),
            Self::AssetMissing => Some(
                Remediation::new("ai.unavailable.asset_missing")
                    .with_action(RemediationAction::Retry),
            ),
            Self::RateLimited => Some(
                Remediation::new("ai.unavailable.rate_limited")
                    .with_action(RemediationAction::Retry),
            ),
            Self::Unknown => None,
        }
    }

    /// Builds the [`AiError`] returned when `start` hits this reason.
    #[must_use]
    pub fn into_error(self) -> AiError {
        let mut error = AiError::new(self.error_code(), format!("backend unavailable: {self:?}"));
        error.remediation = self.remediation();
        error
    }
}

/// Generates text (and structured text via guided generation) from a prompt.
#[async_trait]
pub trait TextGenerator: Send + Sync {
    /// Static capability advertisement.
    fn capabilities(&self) -> TextGenerationCapabilities;

    /// Current OS-level availability.
    async fn availability(&self) -> BackendAvailability;

    /// Begins streaming text for `req`. Returns an error synchronously for an
    /// unavailable backend or an action this backend does not implement;
    /// per-token failures arrive as `Err` items in the returned stream.
    async fn stream_text(
        &self,
        req: TextGenerationRequest,
        cancel: CancellationToken,
    ) -> Result<AiEventStream, AiError>;
}

/// Translates text between languages. Defined for the layered structure; the
/// Apple `TranslationSession` implementation lands with the translate action.
#[async_trait]
pub trait Translator: Send + Sync {
    async fn availability(&self) -> BackendAvailability;
    /// Whether the given source/target pair is installed / requestable.
    async fn pair_status(&self, source: Option<&str>, target: &str) -> BackendAvailability;
    async fn translate(
        &self,
        req: TranslationRequest,
        cancel: CancellationToken,
    ) -> Result<TranslationOutput, AiError>;
}

/// Produces sentence/document embeddings in batches.
///
/// Defined for the layered structure; the `NLContextualEmbedding`
/// implementation lands with semantic search. Batch is mandatory so indexing
/// never degrades to single-only.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn availability(&self) -> BackendAvailability;
    /// Full runtime metadata (identifier / revision / dimension / sequence
    /// cap / languages), read from the model — never baked.
    async fn metadata(&self) -> Result<EmbeddingModelMetadata, AiError>;
    /// Embedding dimensionality, read from the model at runtime (never baked).
    async fn dimension(&self) -> Result<usize, AiError> {
        Ok(self.metadata().await?.dimension)
    }
    async fn embed_batch(
        &self,
        inputs: Vec<EmbeddingInput>,
        cancel: CancellationToken,
    ) -> Result<Vec<EmbeddingVector>, AiError>;
}
