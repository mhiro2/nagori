//! A deterministic [`TextGenerator`] fixture for tests and CI.
//!
//! `MockBackend` streams a canned response one character at a time, honouring
//! cancellation between pulls, and can be constructed in any availability
//! state. It lets the daemon, CLI, and desktop exercise the full engine path —
//! resolver, streaming, cancellation, and every availability branch — on hosts
//! with no Apple Intelligence environment.

use async_trait::async_trait;
use futures::StreamExt;
use nagori_core::AiEvent;
use tokio_util::sync::CancellationToken;

use crate::AiEventStream;
use crate::backend::{
    BackendAvailability, BackendUnavailableReason, Embedder, EmbeddingInput,
    EmbeddingModelMetadata, EmbeddingVector, TextGenerationCapabilities, TextGenerationRequest,
    TextGenerator, TranslationOutput, TranslationRequest, Translator,
};

/// A canned text-generation backend.
#[derive(Debug, Clone)]
pub struct MockBackend {
    availability: BackendAvailability,
    /// Fixed output, or `None` to derive a recognisable string from the input.
    output: Option<String>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            availability: BackendAvailability::Available,
            output: None,
        }
    }
}

impl MockBackend {
    /// An available backend that derives its output from the request input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An available backend that always streams `output`.
    #[must_use]
    pub fn with_output(output: impl Into<String>) -> Self {
        Self {
            availability: BackendAvailability::Available,
            output: Some(output.into()),
        }
    }

    /// A backend that reports the given unavailable `reason`.
    #[must_use]
    pub const fn unavailable(reason: BackendUnavailableReason) -> Self {
        Self {
            availability: BackendAvailability::Unavailable(reason),
            output: None,
        }
    }

    /// Derives a recognisable, action-shaped output from the request so a test
    /// can tell which action it exercised. A fixed `output` (from
    /// [`Self::with_output`]) overrides this.
    fn render(&self, req: &TextGenerationRequest) -> String {
        use nagori_core::AiActionId;
        if let Some(output) = &self.output {
            return output.clone();
        }
        let first = req.input.trim().lines().next().unwrap_or_default().trim();
        match req.action {
            // `ExtractTasks` is prompt-steered toward a Markdown checklist; the
            // mock mirrors that shape so consumers can be tested against it.
            AiActionId::ExtractTasks => format!("- [ ] {first}"),
            AiActionId::Rewrite => format!("Rewrite: {first}"),
            AiActionId::FormatMarkdown => format!("# {first}"),
            AiActionId::ExplainCode => format!("Explanation: {first}"),
            _ => format!("Summary: {first}"),
        }
    }
}

#[async_trait]
impl TextGenerator for MockBackend {
    fn capabilities(&self) -> TextGenerationCapabilities {
        TextGenerationCapabilities {
            streaming: true,
            // Mirrors the Apple backend, whose guided generation is gated on a
            // build toolchain with the `@Generable` macro plugin.
            guided_generation: false,
            on_device: true,
        }
    }

    async fn availability(&self) -> BackendAvailability {
        self.availability
    }

    async fn stream_text(
        &self,
        req: TextGenerationRequest,
        cancel: CancellationToken,
    ) -> Result<AiEventStream, nagori_core::AiError> {
        if let BackendAvailability::Unavailable(reason) = self.availability {
            return Err(reason.into_error());
        }
        Ok(stream_chars(&self.render(&req), cancel))
    }
}

/// A canned [`Translator`] backend for tests and CI.
///
/// Echoes the input back tagged with the target language so a test can assert
/// exactly what the backend received, and can be put into any availability /
/// per-pair state to exercise the engine's translation branch — unavailable
/// provider, a missing language pack, or a clean success — without the Apple
/// Translation framework.
#[derive(Debug, Clone)]
pub struct MockTranslator {
    availability: BackendAvailability,
    /// Status reported for any source/target pair queried via `pair_status`.
    pair: BackendAvailability,
}

impl Default for MockTranslator {
    fn default() -> Self {
        Self {
            availability: BackendAvailability::Available,
            pair: BackendAvailability::Available,
        }
    }
}

impl MockTranslator {
    /// An available translator whose language pairs are all installed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A translator that reports the given unavailable `reason`.
    #[must_use]
    pub const fn unavailable(reason: BackendUnavailableReason) -> Self {
        Self {
            availability: BackendAvailability::Unavailable(reason),
            pair: BackendAvailability::Unavailable(reason),
        }
    }

    /// An available translator whose language packs are not yet installed, so a
    /// `pair_status` query reports [`BackendUnavailableReason::AssetMissing`].
    #[must_use]
    pub const fn pair_missing() -> Self {
        Self {
            availability: BackendAvailability::Available,
            pair: BackendAvailability::Unavailable(BackendUnavailableReason::AssetMissing),
        }
    }
}

#[async_trait]
impl Translator for MockTranslator {
    async fn availability(&self) -> BackendAvailability {
        self.availability
    }

    async fn pair_status(&self, _source: Option<&str>, _target: &str) -> BackendAvailability {
        self.pair
    }

    async fn translate(
        &self,
        req: TranslationRequest,
        cancel: CancellationToken,
    ) -> Result<TranslationOutput, nagori_core::AiError> {
        if let BackendAvailability::Unavailable(reason) = self.availability {
            return Err(reason.into_error());
        }
        if cancel.is_cancelled() {
            return Err(nagori_core::AiError::new(
                nagori_core::AiErrorCode::Unknown,
                "translation cancelled",
            ));
        }
        Ok(TranslationOutput {
            text: format!("[{}] {}", req.target_language, req.input),
            detected_source_language: req.source_language.or_else(|| Some("en".to_owned())),
        })
    }
}

/// A deterministic [`Embedder`] fixture for tests and CI.
///
/// Produces a normalised bag-of-characters vector so texts that share
/// characters land closer in cosine space — enough for semantic-index smoke
/// tests — without the `NLContextualEmbedding` framework. Can be put into any
/// availability state, given a chosen dimension, or stamped with a revision so
/// the index's "revision changed → rebuild" path can be exercised.
#[derive(Debug, Clone)]
pub struct MockEmbedder {
    availability: BackendAvailability,
    dimension: usize,
    revision: u32,
}

impl Default for MockEmbedder {
    fn default() -> Self {
        Self {
            availability: BackendAvailability::Available,
            dimension: 8,
            revision: 1,
        }
    }
}

impl MockEmbedder {
    /// An available embedder with an 8-dimensional space, revision 1.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An available embedder producing `dimension`-wide vectors.
    #[must_use]
    pub fn with_dimension(dimension: usize) -> Self {
        Self {
            dimension: dimension.max(1),
            ..Self::default()
        }
    }

    /// An available embedder stamped with `revision` (for rebuild-on-change
    /// tests).
    #[must_use]
    pub fn with_revision(revision: u32) -> Self {
        Self {
            revision,
            ..Self::default()
        }
    }

    /// An embedder that reports the given unavailable `reason`.
    #[must_use]
    pub const fn unavailable(reason: BackendUnavailableReason) -> Self {
        Self {
            availability: BackendAvailability::Unavailable(reason),
            dimension: 8,
            revision: 1,
        }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0_f32; self.dimension];
        for ch in text.chars() {
            let bucket = (ch as usize) % self.dimension;
            vector[bucket] += 1.0;
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn availability(&self) -> BackendAvailability {
        self.availability
    }

    async fn metadata(&self) -> Result<EmbeddingModelMetadata, nagori_core::AiError> {
        if let BackendAvailability::Unavailable(reason) = self.availability {
            return Err(reason.into_error());
        }
        Ok(EmbeddingModelMetadata {
            model_identifier: "mock-embedder".to_owned(),
            revision: self.revision,
            dimension: self.dimension,
            max_sequence_length: 256,
            languages: vec!["en".to_owned(), "ja".to_owned()],
        })
    }

    async fn embed_batch(
        &self,
        inputs: Vec<EmbeddingInput>,
        cancel: CancellationToken,
    ) -> Result<Vec<EmbeddingVector>, nagori_core::AiError> {
        if let BackendAvailability::Unavailable(reason) = self.availability {
            return Err(reason.into_error());
        }
        if cancel.is_cancelled() {
            return Err(nagori_core::AiError::new(
                nagori_core::AiErrorCode::Unknown,
                "embedding cancelled",
            ));
        }
        Ok(inputs
            .into_iter()
            .map(|input| EmbeddingVector {
                vector: self.embed_one(&input.text),
                id: input.id,
            })
            .collect())
    }
}

/// Internal unfold state for the per-character mock stream.
struct MockState {
    chars: Vec<char>,
    idx: usize,
    seq: u64,
    buf: String,
    cancel: CancellationToken,
    finished: bool,
}

/// Streams `output` one character at a time as `Delta` events, then a terminal
/// `Done` — or `Cancelled` if the token is tripped between pulls.
fn stream_chars(output: &str, cancel: CancellationToken) -> AiEventStream {
    let state = MockState {
        chars: output.chars().collect(),
        idx: 0,
        seq: 0,
        buf: String::new(),
        cancel,
        finished: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        if st.finished {
            return None;
        }
        if st.cancel.is_cancelled() {
            st.finished = true;
            return Some((Ok(AiEvent::Cancelled), st));
        }
        if st.idx >= st.chars.len() {
            st.finished = true;
            let done = AiEvent::Done {
                final_text: std::mem::take(&mut st.buf),
                created_entry: None,
                warnings: Vec::new(),
            };
            return Some((Ok(done), st));
        }
        let ch = st.chars[st.idx];
        st.idx += 1;
        let seq = st.seq;
        st.seq += 1;
        st.buf.push(ch);
        Some((
            Ok(AiEvent::Delta {
                seq,
                text: ch.to_string(),
            }),
            st,
        ))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_core::{AiActionId, AiRequestOptions, RequestId};

    fn request(input: &str) -> TextGenerationRequest {
        request_for(AiActionId::Summarize, input)
    }

    fn request_for(action: AiActionId, input: &str) -> TextGenerationRequest {
        TextGenerationRequest {
            request_id: RequestId::new(),
            action,
            input: input.to_owned(),
            options: AiRequestOptions::default(),
            guided_schema: None,
        }
    }

    async fn collect_final(mut stream: AiEventStream) -> String {
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                AiEvent::Delta { .. } | AiEvent::Replace { .. } => {}
                AiEvent::Done { final_text, .. } => return final_text,
                AiEvent::Cancelled => panic!("unexpected cancel"),
            }
        }
        panic!("stream ended without Done");
    }

    #[tokio::test]
    async fn render_is_action_shaped() {
        let backend = MockBackend::new();
        let tasks = backend
            .stream_text(
                request_for(AiActionId::ExtractTasks, "ship the release"),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Guided extraction renders a Markdown checklist.
        assert_eq!(collect_final(tasks).await, "- [ ] ship the release");

        let rewrite = backend
            .stream_text(
                request_for(AiActionId::Rewrite, "make this better"),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(collect_final(rewrite).await, "Rewrite: make this better");
    }

    #[tokio::test]
    async fn streams_deltas_then_done() {
        let backend = MockBackend::with_output("ab世");
        let cancel = CancellationToken::new();
        let mut stream = backend
            .stream_text(request("ignored"), cancel)
            .await
            .unwrap();

        let mut buf = String::new();
        let mut terminal = None;
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                AiEvent::Delta { text, .. } => buf.push_str(&text),
                AiEvent::Replace { text, .. } => buf = text,
                done @ AiEvent::Done { .. } => {
                    terminal = Some(done);
                    break;
                }
                AiEvent::Cancelled => panic!("unexpected cancel"),
            }
        }
        assert_eq!(buf, "ab世");
        assert_eq!(
            terminal,
            Some(AiEvent::Done {
                final_text: "ab世".to_owned(),
                created_entry: None,
                warnings: Vec::new(),
            })
        );
    }

    #[tokio::test]
    async fn cancellation_before_consume_yields_cancelled() {
        let backend = MockBackend::with_output("x".repeat(100));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut stream = backend
            .stream_text(request("ignored"), cancel)
            .await
            .unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, AiEvent::Cancelled);
    }

    #[tokio::test]
    async fn unavailable_backend_errors_synchronously() {
        let backend = MockBackend::unavailable(BackendUnavailableReason::NotEnabled);
        let cancel = CancellationToken::new();
        // `AiEventStream` is not `Debug`, so use let-else rather than `expect_err`.
        let Err(err) = backend.stream_text(request("ignored"), cancel).await else {
            panic!("unavailable backend must error");
        };
        assert_eq!(err.code, nagori_core::AiErrorCode::Unavailable);
    }

    fn translation_request(input: &str, target: &str) -> TranslationRequest {
        TranslationRequest {
            request_id: RequestId::new(),
            input: input.to_owned(),
            source_language: None,
            target_language: target.to_owned(),
        }
    }

    #[tokio::test]
    async fn translator_echoes_input_tagged_with_target() {
        let backend = MockTranslator::new();
        let out = backend
            .translate(translation_request("hello", "ja"), CancellationToken::new())
            .await
            .expect("available translator should translate");
        assert_eq!(out.text, "[ja] hello");
        // No explicit source means the mock reports its auto-detected default.
        assert_eq!(out.detected_source_language.as_deref(), Some("en"));
    }

    #[tokio::test]
    async fn translator_pair_missing_reports_asset_missing() {
        let backend = MockTranslator::pair_missing();
        assert_eq!(backend.availability().await, BackendAvailability::Available);
        assert_eq!(
            backend.pair_status(Some("en"), "ja").await,
            BackendAvailability::Unavailable(BackendUnavailableReason::AssetMissing)
        );
    }

    #[tokio::test]
    async fn translator_unavailable_errors() {
        let backend = MockTranslator::unavailable(BackendUnavailableReason::NotEnabled);
        let Err(err) = backend
            .translate(translation_request("hi", "ja"), CancellationToken::new())
            .await
        else {
            panic!("unavailable translator must error");
        };
        assert_eq!(err.code, nagori_core::AiErrorCode::Unavailable);
    }

    #[tokio::test]
    async fn embedder_batches_deterministic_unit_vectors() {
        let backend = MockEmbedder::with_dimension(8);
        let meta = backend.metadata().await.unwrap();
        assert_eq!(meta.dimension, 8);
        assert_eq!(backend.dimension().await.unwrap(), 8);

        let inputs = vec![
            EmbeddingInput {
                id: "a".to_owned(),
                text: "hello world".to_owned(),
            },
            EmbeddingInput {
                id: "b".to_owned(),
                text: "hello world".to_owned(),
            },
        ];
        let out = backend
            .embed_batch(inputs, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].vector.len(), 8);
        // Same text → same (deterministic) vector.
        assert_eq!(out[0].vector, out[1].vector);
        // Unit-normalised.
        let norm = out[0].vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn embedder_unavailable_errors() {
        let backend = MockEmbedder::unavailable(BackendUnavailableReason::AssetMissing);
        let Err(err) = backend.metadata().await else {
            panic!("unavailable embedder must error on metadata");
        };
        assert_eq!(err.code, nagori_core::AiErrorCode::AssetMissing);
    }
}
