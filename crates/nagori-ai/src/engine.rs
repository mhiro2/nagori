//! The upper-tier [`AiActionEngine`] the daemon drives, plus its provider-bound
//! concrete implementation [`AiEngine`].
//!
//! The engine resolves an action to a backend family via the `ActionSpec`
//! table, checks the backend's OS availability, and returns a stream of
//! [`AiEvent`]s. It owns no cancellation state: the caller (the daemon's
//! request registry, or the CLI's local driver) creates the
//! [`CancellationToken`] and keeps it, so the engine can stay a thin dispatch
//! layer.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use nagori_core::{
    AiActionId, AiActionRequest, AiAvailabilityReport, AiCapability, AiCapabilitySet, AiError,
    AiErrorCode, AiEvent, AiOverallStatus, AiProviderKind, AiSettings, PerActionAvailability,
    PerActionStatus, Remediation, RequestId, SemanticIndexAvailability,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::backend::{
    BackendAvailability, Embedder, TextGenerationRequest, TextGenerator, TranslationRequest,
    Translator,
};
use crate::resolver::{BackendKind, resolve_backend};

/// A stream of streaming events for one AI action.
///
/// The terminal item is exactly one of `Ok(AiEvent::Done)`,
/// `Ok(AiEvent::Cancelled)`, or `Err(AiError)`.
pub type AiEventStream = BoxStream<'static, Result<AiEvent, AiError>>;

/// A started AI action: its id plus the live event stream. Deliberately holds
/// no cancellation handle — the caller owns the [`CancellationToken`] it passed
/// to [`AiActionEngine::start`].
pub struct AiActionRun {
    pub request_id: RequestId,
    pub events: AiEventStream,
}

/// The engine the daemon and CLI drive. Resolves actions to backends, reports
/// availability, and advertises capabilities.
#[async_trait]
pub trait AiActionEngine: Send + Sync {
    /// The provider family this engine resolves actions under.
    fn provider(&self) -> AiProviderKind;

    /// Begins an action, returning its event stream. Synchronous failures
    /// (unavailable provider, capability mismatch) come back as `Err`;
    /// mid-stream failures arrive as `Err` items inside [`AiActionRun::events`].
    async fn start(
        &self,
        req: AiActionRequest,
        cancel: CancellationToken,
    ) -> Result<AiActionRun, AiError>;

    /// Builds a point-in-time availability report, gated by `settings`.
    async fn availability(&self, settings: &AiSettings) -> AiAvailabilityReport;

    /// The union of technical capabilities the wired backends expose.
    fn capabilities(&self) -> AiCapabilitySet;

    /// The embedding backend, when one is wired. The daemon's semantic index
    /// pipeline drives this directly (embedding is not an `AiActionId`-level
    /// streaming action) to embed queries and build the index.
    fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        None
    }
}

/// A provider-bound engine. Built for exactly one [`AiProviderKind`] with that
/// provider's backends; the daemon selects which engine to drive based on the
/// active settings.
#[derive(Clone)]
pub struct AiEngine {
    provider: AiProviderKind,
    text_generator: Option<Arc<dyn TextGenerator>>,
    translator: Option<Arc<dyn Translator>>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl AiEngine {
    /// Starts a builder for an engine bound to `provider`.
    #[must_use]
    pub fn builder(provider: AiProviderKind) -> AiEngineBuilder {
        AiEngineBuilder {
            provider,
            text_generator: None,
            translator: None,
            embedder: None,
        }
    }

    /// The provider family this engine is wired for.
    #[must_use]
    pub const fn provider(&self) -> AiProviderKind {
        self.provider
    }

    /// Resolves the per-action status used by both `availability` and the
    /// desktop's gating. The two availabilities are the (cached) text-generation
    /// and translation backend availabilities, so the caller probes the OS at
    /// most once per backend family.
    fn action_status(
        &self,
        action: AiActionId,
        settings: &AiSettings,
        tg_availability: Option<BackendAvailability>,
        translator_availability: Option<BackendAvailability>,
    ) -> PerActionAvailability {
        let (status, remediation) =
            self.resolve_status(action, settings, tg_availability, translator_availability);
        PerActionAvailability {
            action,
            status,
            remediation,
        }
    }

    fn resolve_status(
        &self,
        action: AiActionId,
        settings: &AiSettings,
        tg_availability: Option<BackendAvailability>,
        translator_availability: Option<BackendAvailability>,
    ) -> (PerActionStatus, Option<Remediation>) {
        if !settings.enabled {
            return (PerActionStatus::DisabledBySettings, None);
        }
        if settings.provider == AiProviderKind::Disabled {
            return (PerActionStatus::NotConfigured, None);
        }
        if settings.provider != self.provider {
            // A real provider is selected but it is not this engine's family,
            // and no backend for it is wired.
            return (PerActionStatus::CapabilityMismatch, None);
        }
        if !settings.allowed_actions.is_empty() && !settings.allowed_actions.contains(&action) {
            return (PerActionStatus::DisabledBySettings, None);
        }
        let Some(kind) = resolve_backend(action, self.provider) else {
            return (PerActionStatus::CapabilityMismatch, None);
        };
        match kind {
            BackendKind::TextGeneration => match (self.text_generator.is_some(), tg_availability) {
                (false, _) | (true, None) => (PerActionStatus::CapabilityMismatch, None),
                (true, Some(BackendAvailability::Available)) => (PerActionStatus::Available, None),
                (true, Some(BackendAvailability::Unavailable(reason))) => {
                    (os_unavailable_status(reason), reason.remediation())
                }
            },
            BackendKind::Translation => {
                match (self.translator.is_some(), translator_availability) {
                    (false, _) | (true, None) => (PerActionStatus::CapabilityMismatch, None),
                    (true, Some(BackendAvailability::Available)) => {
                        (PerActionStatus::Available, None)
                    }
                    (true, Some(BackendAvailability::Unavailable(reason))) => {
                        (os_unavailable_status(reason), reason.remediation())
                    }
                }
            }
            // No `AiActionId` resolves to the embedding backend (semantic
            // search is driven directly by the daemon, not through `start`),
            // so this arm is only reachable if a future `ActionSpec` maps an
            // action here without an embedder wired.
            BackendKind::Embedding => (PerActionStatus::CapabilityMismatch, None),
        }
    }
}

/// Adapts a (non-streaming) [`Translator::translate`] into the engine's
/// [`AiEventStream`]: one terminal item — `Ok(Done)` on success, `Ok(Cancelled)`
/// if the caller's token trips first, or `Err(AiError)` if translation fails.
///
/// The translate call is deferred to the first poll (the stream is lazy), so the
/// daemon's consumer-side deadline race still bounds it, and the cancel token is
/// passed through so the backend can abort its in-flight work.
fn translation_stream(
    backend: Arc<dyn Translator>,
    req: TranslationRequest,
    cancel: CancellationToken,
) -> AiEventStream {
    use futures::future::{Either, select};
    use futures::{FutureExt, StreamExt};

    let translate_cancel = cancel.clone();
    futures::stream::once(async move {
        // Pre-check: `select` polls the translate future first and returns as soon
        // as it is ready, so a synchronous backend would never reach the cancel
        // arm. Short-circuit an already-cancelled request before any work runs.
        if cancel.is_cancelled() {
            return Ok(AiEvent::Cancelled);
        }
        let translate = backend.translate(req, translate_cancel).boxed();
        let cancelled = cancel.cancelled().boxed();
        match select(translate, cancelled).await {
            Either::Left((result, _)) => result.map(|out| AiEvent::Done {
                final_text: out.text,
                created_entry: None,
                warnings: Vec::new(),
            }),
            Either::Right(((), _)) => Ok(AiEvent::Cancelled),
        }
    })
    .boxed()
}

/// Maps an OS-unavailable backend reason onto the per-action availability
/// status.
const fn os_unavailable_status(
    reason: crate::backend::BackendUnavailableReason,
) -> PerActionStatus {
    use crate::backend::BackendUnavailableReason as R;
    match reason {
        R::AssetMissing => PerActionStatus::AssetMissing,
        R::Unknown => PerActionStatus::Unknown,
        _ => PerActionStatus::OsUnavailable,
    }
}

#[async_trait]
impl AiActionEngine for AiEngine {
    fn provider(&self) -> AiProviderKind {
        self.provider
    }

    async fn start(
        &self,
        req: AiActionRequest,
        cancel: CancellationToken,
    ) -> Result<AiActionRun, AiError> {
        if self.provider == AiProviderKind::Disabled {
            return Err(AiError::new(
                AiErrorCode::Unavailable,
                "no AI provider configured",
            ));
        }
        let kind = resolve_backend(req.action, self.provider).ok_or_else(|| {
            AiError::new(
                AiErrorCode::CapabilityMismatch,
                format!(
                    "no backend wired for {:?} under {:?}",
                    req.action, self.provider
                ),
            )
        })?;

        match kind {
            BackendKind::TextGeneration => {
                let backend = self.text_generator.as_ref().ok_or_else(|| {
                    AiError::new(
                        AiErrorCode::CapabilityMismatch,
                        "text generation backend not configured",
                    )
                })?;
                if let BackendAvailability::Unavailable(reason) = backend.availability().await {
                    return Err(reason.into_error());
                }
                let request_id = req.request_id;
                let tg_req = TextGenerationRequest {
                    request_id,
                    action: req.action,
                    guided_schema: req.options.guided_schema,
                    input: req.input,
                    options: req.options,
                };
                let events = backend.stream_text(tg_req, cancel).await?;
                Ok(AiActionRun { request_id, events })
            }
            BackendKind::Translation => {
                let backend = self.translator.clone().ok_or_else(|| {
                    AiError::new(
                        AiErrorCode::CapabilityMismatch,
                        "translation backend not configured",
                    )
                })?;
                if let BackendAvailability::Unavailable(reason) = backend.availability().await {
                    return Err(reason.into_error());
                }
                let Some(target_language) = req.options.target_language.clone() else {
                    return Err(AiError::new(
                        AiErrorCode::CapabilityMismatch,
                        "translate requires a target language",
                    ));
                };
                let request_id = req.request_id;
                let tr_req = TranslationRequest {
                    request_id,
                    input: req.input,
                    source_language: req.options.source_language,
                    target_language,
                };
                let events = translation_stream(backend, tr_req, cancel);
                Ok(AiActionRun { request_id, events })
            }
            BackendKind::Embedding => {
                let _ = self.embedder.as_ref();
                Err(AiError::new(
                    AiErrorCode::CapabilityMismatch,
                    "embedding is not yet implemented",
                ))
            }
        }
    }

    async fn availability(&self, settings: &AiSettings) -> AiAvailabilityReport {
        let tg_availability = match &self.text_generator {
            Some(backend) => Some(backend.availability().await),
            None => None,
        };
        let translator_availability = match &self.translator {
            Some(backend) => Some(backend.availability().await),
            None => None,
        };

        let per_action: Vec<PerActionAvailability> = AiActionId::all()
            .iter()
            .map(|&action| {
                self.action_status(action, settings, tg_availability, translator_availability)
            })
            .collect();

        let overall_status = if !settings.enabled {
            AiOverallStatus::Disabled
        } else if per_action
            .iter()
            .any(|entry| entry.status == PerActionStatus::Available)
        {
            AiOverallStatus::Available
        } else {
            AiOverallStatus::Unavailable
        };

        let semantic_index = if settings.semantic_index_enabled {
            match &self.embedder {
                None => SemanticIndexAvailability::NotImplemented,
                Some(backend) => match backend.availability().await {
                    BackendAvailability::Available => SemanticIndexAvailability::Available,
                    BackendAvailability::Unavailable(_) => SemanticIndexAvailability::Unavailable,
                },
            }
        } else {
            SemanticIndexAvailability::Disabled
        };

        AiAvailabilityReport {
            generated_at: OffsetDateTime::now_utc(),
            // Echo the *selected* provider, not this engine's wired family, so a
            // provider mismatch reads as "you chose X (which has no backend)"
            // rather than mislabelling every action under this engine's family.
            provider: settings.provider,
            overall_status,
            per_action,
            semantic_index,
        }
    }

    fn capabilities(&self) -> AiCapabilitySet {
        let mut caps = BTreeSet::new();
        if let Some(backend) = &self.text_generator {
            let c = backend.capabilities();
            caps.insert(AiCapability::TextGeneration);
            if c.streaming {
                caps.insert(AiCapability::StreamingText);
            }
            if c.guided_generation {
                caps.insert(AiCapability::GuidedGeneration);
            }
            if c.on_device {
                caps.insert(AiCapability::OnDevice);
            }
        }
        if self.translator.is_some() {
            caps.insert(AiCapability::Translation);
            caps.insert(AiCapability::LanguagePairMatrix);
        }
        if self.embedder.is_some() {
            caps.insert(AiCapability::EmbeddingBatch);
            caps.insert(AiCapability::RequiresAssets);
        }
        AiCapabilitySet(caps)
    }

    fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        self.embedder.clone()
    }
}

/// Builder for [`AiEngine`].
pub struct AiEngineBuilder {
    provider: AiProviderKind,
    text_generator: Option<Arc<dyn TextGenerator>>,
    translator: Option<Arc<dyn Translator>>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl AiEngineBuilder {
    #[must_use]
    pub fn text_generator(mut self, backend: Arc<dyn TextGenerator>) -> Self {
        self.text_generator = Some(backend);
        self
    }

    #[must_use]
    pub fn translator(mut self, backend: Arc<dyn Translator>) -> Self {
        self.translator = Some(backend);
        self
    }

    #[must_use]
    pub fn embedder(mut self, backend: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(backend);
        self
    }

    #[must_use]
    pub fn build(self) -> AiEngine {
        AiEngine {
            provider: self.provider,
            text_generator: self.text_generator,
            translator: self.translator,
            embedder: self.embedder,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendUnavailableReason;
    use crate::mock::{MockBackend, MockTranslator};
    use futures::StreamExt;
    use nagori_core::AiRequestOptions;

    fn settings(enabled: bool, provider: AiProviderKind) -> AiSettings {
        AiSettings {
            enabled,
            provider,
            ..AiSettings::default()
        }
    }

    fn request(action: AiActionId, input: &str) -> AiActionRequest {
        AiActionRequest {
            request_id: RequestId::new(),
            action,
            input: input.to_owned(),
            policy: action.input_policy(),
            options: AiRequestOptions::default(),
        }
    }

    fn apple_engine(backend: MockBackend) -> AiEngine {
        AiEngine::builder(AiProviderKind::AppleNative)
            .text_generator(Arc::new(backend))
            .build()
    }

    fn apple_engine_with_translator(translator: MockTranslator) -> AiEngine {
        AiEngine::builder(AiProviderKind::AppleNative)
            .text_generator(Arc::new(MockBackend::new()))
            .translator(Arc::new(translator))
            .build()
    }

    fn translate_request(input: &str, target: &str) -> AiActionRequest {
        AiActionRequest {
            request_id: RequestId::new(),
            action: AiActionId::Translate,
            input: input.to_owned(),
            policy: AiActionId::Translate.input_policy(),
            options: AiRequestOptions {
                target_language: Some(target.to_owned()),
                ..AiRequestOptions::default()
            },
        }
    }

    async fn drain_to_done(mut events: AiEventStream) -> AiEvent {
        while let Some(item) = events.next().await {
            let event = item.expect("no stream error");
            if event.is_terminal() {
                return event;
            }
        }
        panic!("stream ended without a terminal event");
    }

    #[tokio::test]
    async fn start_summarize_streams_to_done() {
        let engine = apple_engine(MockBackend::with_output("Hi 世界"));
        let cancel = CancellationToken::new();
        let run = engine
            .start(request(AiActionId::Summarize, "anything"), cancel)
            .await
            .expect("summarize should start");

        let mut buf = String::new();
        let mut events = run.events;
        let mut saw_done = false;
        while let Some(item) = events.next().await {
            match item.expect("no stream error") {
                AiEvent::Delta { text, .. } => buf.push_str(&text),
                AiEvent::Replace { text, .. } => buf = text,
                AiEvent::Done { final_text, .. } => {
                    assert_eq!(final_text, "Hi 世界");
                    saw_done = true;
                    break;
                }
                AiEvent::Cancelled => panic!("unexpected cancel"),
            }
        }
        assert!(saw_done);
        assert_eq!(buf, "Hi 世界");
    }

    #[tokio::test]
    async fn start_text_generation_actions_stream_to_done() {
        let engine = apple_engine(MockBackend::new());
        for action in [
            AiActionId::Summarize,
            AiActionId::Rewrite,
            AiActionId::FormatMarkdown,
            AiActionId::ExtractTasks,
            AiActionId::ExplainCode,
        ] {
            let run = engine
                .start(request(action, "do the thing"), CancellationToken::new())
                .await
                .unwrap_or_else(|_| panic!("{action:?} should start"));
            match drain_to_done(run.events).await {
                AiEvent::Done { final_text, .. } => {
                    assert!(!final_text.is_empty(), "{action:?} produced no text");
                }
                other => panic!("{action:?} expected Done, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn start_translate_streams_to_done() {
        let engine = apple_engine_with_translator(MockTranslator::new());
        let run = engine
            .start(translate_request("hello", "ja"), CancellationToken::new())
            .await
            .expect("translate should start");
        match drain_to_done(run.events).await {
            AiEvent::Done { final_text, .. } => assert_eq!(final_text, "[ja] hello"),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_translate_without_target_is_capability_mismatch() {
        let engine = apple_engine_with_translator(MockTranslator::new());
        // A translate request whose options carry no target language.
        let req = request(AiActionId::Translate, "hello");
        let Err(err) = engine.start(req, CancellationToken::new()).await else {
            panic!("translate without a target language must error");
        };
        assert_eq!(err.code, AiErrorCode::CapabilityMismatch);
    }

    #[tokio::test]
    async fn start_translate_without_backend_is_capability_mismatch() {
        // Engine wired for Apple text generation only — no translator.
        let engine = apple_engine(MockBackend::new());
        let Err(err) = engine
            .start(translate_request("hello", "ja"), CancellationToken::new())
            .await
        else {
            panic!("translate has no backend wired");
        };
        assert_eq!(err.code, AiErrorCode::CapabilityMismatch);
    }

    #[tokio::test]
    async fn start_translate_cancelled_before_consume_yields_cancelled() {
        let engine = apple_engine_with_translator(MockTranslator::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let run = engine
            .start(translate_request("hello", "ja"), cancel)
            .await
            .expect("translate should still start so the cancel can drain");
        assert_eq!(drain_to_done(run.events).await, AiEvent::Cancelled);
    }

    #[tokio::test]
    async fn start_translate_unavailable_surfaces_remediation() {
        let engine = apple_engine_with_translator(MockTranslator::unavailable(
            BackendUnavailableReason::NotEnabled,
        ));
        let Err(err) = engine
            .start(translate_request("hello", "ja"), CancellationToken::new())
            .await
        else {
            panic!("translator is unavailable");
        };
        assert_eq!(err.code, AiErrorCode::Unavailable);
        assert!(err.remediation.is_some());
    }

    #[tokio::test]
    async fn availability_reports_translate_when_translator_wired() {
        let engine = apple_engine_with_translator(MockTranslator::new());
        let report = engine
            .availability(&settings(true, AiProviderKind::AppleNative))
            .await;
        let translate = report
            .per_action
            .iter()
            .find(|entry| entry.action == AiActionId::Translate)
            .unwrap();
        assert_eq!(translate.status, PerActionStatus::Available);
    }

    #[tokio::test]
    async fn start_unavailable_backend_surfaces_remediation() {
        let engine = apple_engine(MockBackend::unavailable(
            BackendUnavailableReason::NotEnabled,
        ));
        let cancel = CancellationToken::new();
        let Err(err) = engine
            .start(request(AiActionId::Summarize, "x"), cancel)
            .await
        else {
            panic!("backend is unavailable");
        };
        assert_eq!(err.code, AiErrorCode::Unavailable);
        assert!(err.remediation.is_some());
    }

    #[tokio::test]
    async fn availability_reports_summarize_available_when_enabled() {
        let engine = apple_engine(MockBackend::new());
        let report = engine
            .availability(&settings(true, AiProviderKind::AppleNative))
            .await;
        assert_eq!(report.overall_status, AiOverallStatus::Available);
        let summarize = report
            .per_action
            .iter()
            .find(|entry| entry.action == AiActionId::Summarize)
            .unwrap();
        assert_eq!(summarize.status, PerActionStatus::Available);
        // Every text-generation action is wired, so it is available too.
        let rewrite = report
            .per_action
            .iter()
            .find(|entry| entry.action == AiActionId::Rewrite)
            .unwrap();
        assert_eq!(rewrite.status, PerActionStatus::Available);
        // Translate has no translator wired on this text-only engine, so it
        // still reports a capability mismatch even when AI is on.
        let translate = report
            .per_action
            .iter()
            .find(|entry| entry.action == AiActionId::Translate)
            .unwrap();
        assert_eq!(translate.status, PerActionStatus::CapabilityMismatch);
    }

    #[tokio::test]
    async fn availability_is_disabled_when_master_toggle_off() {
        let engine = apple_engine(MockBackend::new());
        let report = engine
            .availability(&settings(false, AiProviderKind::AppleNative))
            .await;
        assert_eq!(report.overall_status, AiOverallStatus::Disabled);
        assert!(
            report
                .per_action
                .iter()
                .all(|entry| entry.status == PerActionStatus::DisabledBySettings)
        );
    }

    #[tokio::test]
    async fn availability_os_unavailable_carries_remediation() {
        let engine = apple_engine(MockBackend::unavailable(
            BackendUnavailableReason::NotEnabled,
        ));
        let report = engine
            .availability(&settings(true, AiProviderKind::AppleNative))
            .await;
        assert_eq!(report.overall_status, AiOverallStatus::Unavailable);
        let summarize = report
            .per_action
            .iter()
            .find(|entry| entry.action == AiActionId::Summarize)
            .unwrap();
        assert_eq!(summarize.status, PerActionStatus::OsUnavailable);
        assert!(summarize.remediation.is_some());
    }

    #[tokio::test]
    async fn capabilities_reflect_text_generator() {
        let engine = apple_engine(MockBackend::new());
        let caps = engine.capabilities();
        assert!(caps.contains(AiCapability::TextGeneration));
        assert!(caps.contains(AiCapability::StreamingText));
        assert!(caps.contains(AiCapability::OnDevice));
        assert!(!caps.contains(AiCapability::Translation));
    }
}
