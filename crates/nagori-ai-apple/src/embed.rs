//! The Apple `NLContextualEmbedding` backend.
//!
//! Implements `nagori-ai`'s [`Embedder`] over the Swift bridge so the daemon can
//! inject it into an `AiEngine` without taking any Apple dependency itself.
//!
//! `NLContextualEmbedding` uses different models for different language groups
//! (e.g. Latin-script vs. CJK), and those produce *incompatible* embedding
//! spaces. To keep one coherent index, the backend is pinned to a single
//! language/model at construction: every clip is embedded with that one model,
//! so all stored vectors are comparable. The model identity is surfaced through
//! [`Embedder::metadata`]; if the configured language (and therefore the model)
//! changes, the daemon detects the metadata mismatch and rebuilds the index.

use async_trait::async_trait;
use nagori_ai::{
    BackendAvailability, BackendUnavailableReason, Embedder, EmbeddingInput,
    EmbeddingModelMetadata, EmbeddingVector,
};
use nagori_core::{AiError, AiErrorCode};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::bridge;

/// Upper bound on characters embedded per entry. Longer inputs are truncated
/// explicitly (never silently): semantic ranking of a clip does not improve from
/// embedding pages of text, and this bounds the work per entry.
const MAX_EMBED_CHARS: usize = 4_000;

/// The user's preferred language code (e.g. `"en"`, `"ja"`), or `"en"` if it
/// cannot be resolved.
///
/// The daemon pins [`AppleEmbedderBackend`] to this so the index is built with
/// the model for the language the user's clips are most likely in.
#[must_use]
pub fn preferred_embedding_language() -> String {
    bridge::preferred_language()
}

/// Embedding backed by `NLContextualEmbedding` via the Swift bridge, pinned to
/// one language/model.
pub struct AppleEmbedderBackend {
    language: String,
    /// Model metadata is immutable per language, so it is read once and cached.
    metadata: OnceCell<EmbeddingModelMetadata>,
}

impl AppleEmbedderBackend {
    /// Creates a backend pinned to `language` (a locale identifier like `"en"`
    /// or `"ja"`). An empty language falls back to `"en"`.
    #[must_use]
    pub fn new(language: impl Into<String>) -> Self {
        let language = language.into();
        let language = if language.trim().is_empty() {
            "en".to_owned()
        } else {
            language
        };
        Self {
            language,
            metadata: OnceCell::new(),
        }
    }

    /// The locale identifier this backend embeds with.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Requests download of this language's embedding assets (used when enabling
    /// the index / onboarding). Resolves once the download finishes or fails.
    pub async fn request_assets(&self) -> Result<(), AiError> {
        bridge::embed_request_assets(&self.language).await
    }

    async fn cached_metadata(&self) -> Result<&EmbeddingModelMetadata, AiError> {
        self.metadata
            .get_or_try_init(|| async {
                let raw = bridge::embed_metadata(&self.language).await?;
                Ok(EmbeddingModelMetadata {
                    model_identifier: raw.model_identifier,
                    revision: raw.revision,
                    dimension: raw.dimension,
                    max_sequence_length: raw.max_sequence_length,
                    languages: vec![self.language.clone()],
                })
            })
            .await
    }

    /// Embeds one text, chunking it to the model's sequence cap and mean-pooling
    /// the chunk vectors so the tail of a long clip is never silently dropped.
    async fn embed_pooled(
        &self,
        text: &str,
        max_chars: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<f32>, AiError> {
        let chunks = chunk_chars(text, max_chars, MAX_EMBED_CHARS);
        match chunks.as_slice() {
            [] => Err(AiError::new(
                AiErrorCode::BackendInternal,
                "there was nothing to embed",
            )),
            [single] => self.embed_chunk(single, cancel).await,
            many => {
                let mut vectors = Vec::with_capacity(many.len());
                for chunk in many {
                    vectors.push(self.embed_chunk(chunk, cancel).await?);
                }
                mean_pool(&vectors).ok_or_else(|| {
                    AiError::new(
                        AiErrorCode::BackendInternal,
                        "the embedding model produced no usable vectors",
                    )
                })
            }
        }
    }

    /// Embeds one chunk, racing the Swift call against `cancel` so a shutdown is
    /// observed mid-call. The abandoned Swift task still reclaims its context via
    /// its own timeout, so dropping the future here is safe.
    async fn embed_chunk(
        &self,
        chunk: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<f32>, AiError> {
        tokio::select! {
            result = bridge::embed_text(&self.language, chunk) => result,
            () = cancel.cancelled() => {
                Err(AiError::new(AiErrorCode::Unknown, "embedding cancelled"))
            }
        }
    }
}

#[async_trait]
impl Embedder for AppleEmbedderBackend {
    async fn availability(&self) -> BackendAvailability {
        match bridge::embed_availability(&self.language) {
            0 => BackendAvailability::Available,
            1 => BackendAvailability::Unavailable(BackendUnavailableReason::AssetMissing),
            // No model for the language, or an unrecognised state.
            _ => BackendAvailability::Unavailable(BackendUnavailableReason::Unknown),
        }
    }

    async fn metadata(&self) -> Result<EmbeddingModelMetadata, AiError> {
        self.cached_metadata().await.cloned()
    }

    async fn embed_batch(
        &self,
        inputs: Vec<EmbeddingInput>,
        cancel: CancellationToken,
    ) -> Result<Vec<EmbeddingVector>, AiError> {
        let max_chars = self.cached_metadata().await?.max_sequence_length.max(1);
        let mut out = Vec::with_capacity(inputs.len());
        for input in inputs {
            if cancel.is_cancelled() {
                return Err(AiError::new(AiErrorCode::Unknown, "embedding cancelled"));
            }
            let vector = self.embed_pooled(&input.text, max_chars, &cancel).await?;
            out.push(EmbeddingVector {
                id: input.id,
                vector,
            });
        }
        Ok(out)
    }
}

/// Splits `text` into char-boundary chunks of at most `max_chars`, capping the
/// total characters consumed at `total_cap`. Returns no chunks for empty input.
fn chunk_chars(text: &str, max_chars: usize, total_cap: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0_usize;
    // `take(total_cap)` bounds the total characters consumed without a manual
    // monotonic counter; `current_len` only tracks the in-progress chunk.
    for ch in text.chars().take(total_cap) {
        current.push(ch);
        current_len += 1;
        if current_len >= max_chars {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Mean-pools equal-length vectors into one L2-normalised vector. Returns `None`
/// if there are no vectors of a consistent, non-zero dimension.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn mean_pool(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dimension = vectors.first()?.len();
    if dimension == 0 {
        return None;
    }
    let mut sum = vec![0.0_f64; dimension];
    let mut count = 0_usize;
    for vector in vectors {
        if vector.len() != dimension {
            continue;
        }
        for (slot, value) in sum.iter_mut().zip(vector) {
            *slot += f64::from(*value);
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let mut pooled: Vec<f32> = sum.iter().map(|s| (s / count as f64) as f32).collect();
    let norm = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut pooled {
            *value /= norm;
        }
    }
    Some(pooled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_chars_splits_on_char_boundaries() {
        // Multibyte input must split on char boundaries, not bytes.
        let chunks = chunk_chars("あいうえお", 2, 100);
        assert_eq!(chunks, vec!["あい", "うえ", "お"]);
    }

    #[test]
    fn chunk_chars_caps_total() {
        let chunks = chunk_chars("abcdefghij", 3, 5);
        // Only the first 5 chars are consumed: "abc" + "de".
        assert_eq!(chunks, vec!["abc", "de"]);
    }

    #[test]
    fn chunk_chars_empty_input_yields_no_chunks() {
        assert!(chunk_chars("", 10, 100).is_empty());
    }

    #[test]
    fn mean_pool_averages_and_normalises() {
        let pooled = mean_pool(&[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        // Average is (0.5, 0.5); normalised both components equal 1/√2.
        let expected = 1.0_f32 / 2.0_f32.sqrt();
        assert!((pooled[0] - expected).abs() < 1e-6);
        assert!((pooled[1] - expected).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_skips_mismatched_and_empty() {
        assert!(mean_pool(&[]).is_none());
        assert!(mean_pool(&[Vec::new()]).is_none());
        // A mismatched-length vector is ignored, leaving one valid vector.
        let pooled = mean_pool(&[vec![1.0, 0.0], vec![1.0, 2.0, 3.0]]).unwrap();
        assert_eq!(pooled, vec![1.0, 0.0]);
    }

    #[test]
    fn new_falls_back_to_english_for_blank_language() {
        assert_eq!(AppleEmbedderBackend::new("  ").language(), "en");
        assert_eq!(AppleEmbedderBackend::new("ja").language(), "ja");
    }

    /// End-to-end smoke test against the live `NLContextualEmbedding` model.
    /// Ignored by default: requires the embedding assets to be installed, which
    /// they are not on CI / un-provisioned hosts — `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires installed NLContextualEmbedding assets"]
    async fn real_embed_returns_unit_vector() {
        let backend = AppleEmbedderBackend::new("en");
        if backend.availability().await != BackendAvailability::Available {
            backend.request_assets().await.expect("asset download");
        }
        let meta = backend.metadata().await.expect("metadata");
        let out = backend
            .embed_batch(
                vec![EmbeddingInput {
                    id: "a".to_owned(),
                    text: "hello semantic world".to_owned(),
                }],
                CancellationToken::new(),
            )
            .await
            .expect("embed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].vector.len(), meta.dimension);
        let norm = out[0].vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3);
    }
}
