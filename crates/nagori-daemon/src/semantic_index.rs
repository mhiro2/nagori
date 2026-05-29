//! Background semantic-index pipeline.
//!
//! A single worker task keeps the on-device embedding index in step with the
//! corpus: it embeds newly-captured clips, backfills history, and rebuilds the
//! whole index when the embedding model changes. The vectors live in
//! `nagori-storage` (the `sqlite-vec` backend); the embedder is the wired
//! `nagori-ai` [`Embedder`]. This module is the glue plus the operational
//! guards — battery, batching, rate-limit backoff, pause/resume, and progress.
//!
//! Semantic *queries* (the read path) are handled inline by
//! [`NagoriRuntime::search`]; this module owns the *write* path.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nagori_ai::{Embedder, EmbeddingInput};
use nagori_core::{
    AppError, EntryId, Result, SearchQuery, SearchResult, SemanticIndexMeta, SemanticIndexState,
    SemanticIndexStatus, SettingsRepository, normalize_text,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::runtime::{NagoriRuntime, ShutdownHandle};

/// Probe returning whether the host is on AC power: `Some(true)` on AC,
/// `Some(false)` on battery, `None` if it cannot be determined.
pub type PowerProbe = std::sync::Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// Bumped when the indexing pipeline's content shaping changes in a way that
/// invalidates previously-stored vectors for the same model.
const INDEX_VERSION: u32 = 1;

/// Entries embedded per `embed_batch` call. Small so each batch stays
/// cancellable and a settings/power change is observed promptly between batches.
const EMBED_BATCH: usize = 16;

/// Idle re-check cadence. Covers AC-power changes, late asset downloads, and any
/// missed wake so backfill always makes progress without a capture.
const IDLE_TICK: Duration = Duration::from_mins(1);

/// Backoff schedule applied after a rate-limited / timed-out batch before the
/// next attempt, capped so a wedged model never spins.
const BACKOFF_STEPS_MS: &[u64] = &[1_000, 2_000, 4_000, 8_000, 16_000];

/// Shared state for the background semantic-index worker.
///
/// Lives on the runtime (wrapped in `Arc`) so the capture notifier, IPC
/// handlers, and the worker all see the same wake signal, coarse state, and
/// rebuild flag.
pub struct SemanticState {
    /// Fired by a capture (or an enable / rebuild request) to wake the worker.
    wake: Notify,
    /// The worker's current coarse state, read by `semantic_index_status`.
    state: Mutex<SemanticIndexState>,
    /// Set by `rebuild_semantic_index`; the worker clears the index and
    /// re-embeds everything, then resets the flag.
    rebuild_requested: AtomicBool,
    /// AC-power probe for the battery guard; `None` means "treat as unknown".
    power: Option<PowerProbe>,
}

impl SemanticState {
    #[must_use]
    pub fn new(power: Option<PowerProbe>) -> Self {
        Self {
            wake: Notify::new(),
            state: Mutex::new(SemanticIndexState::Disabled),
            rebuild_requested: AtomicBool::new(false),
            power,
        }
    }

    fn set_state(&self, state: SemanticIndexState) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    fn current_state(&self) -> SemanticIndexState {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the battery guard permits running now (`true` on AC or unknown).
    fn power_allows(&self, ac_power_only: bool) -> bool {
        if !ac_power_only {
            return true;
        }
        // Unknown power state (`None`) errs toward running so a host without a
        // probe is not stuck never indexing.
        self.power
            .as_ref()
            .is_none_or(|probe| probe().unwrap_or(true))
    }
}

impl NagoriRuntime {
    /// Wakes the semantic indexer after a capture so the new clip is embedded
    /// promptly. A no-op beyond signalling; the worker pulls pending work from
    /// the store.
    pub fn notify_semantic_capture(&self) {
        self.semantic.wake.notify_one();
    }

    /// Requests a full rebuild of the semantic index (e.g. after the user asks
    /// to re-index). The worker clears the stored vectors and re-embeds the
    /// whole corpus on its next pass.
    pub fn rebuild_semantic_index(&self) {
        self.semantic
            .rebuild_requested
            .store(true, Ordering::SeqCst);
        self.semantic.wake.notify_one();
    }

    /// A point-in-time snapshot of the semantic index for the UI / doctor.
    pub async fn semantic_index_status(&self) -> Result<SemanticIndexStatus> {
        let settings = self.store.get_settings().await?.ai;
        if !settings.semantic_index_enabled {
            return Ok(SemanticIndexStatus::disabled());
        }
        let Some(_embedder) = self.embedder() else {
            return Ok(SemanticIndexStatus::unsupported());
        };
        let counts = self.store.semantic_counts().await?;
        let model = self.store.semantic_meta().await?;
        Ok(SemanticIndexStatus {
            state: self.semantic.current_state(),
            indexed: counts.indexed,
            pending: counts.total.saturating_sub(counts.indexed),
            total: counts.total,
            model,
        })
    }

    /// Embeds `query` and ranks the stored vectors against it. Surfaces a clear
    /// `Unsupported` error when the index is disabled or the embedder is not
    /// available so the caller can fall back or explain why.
    pub(crate) async fn semantic_search_results(
        &self,
        query: SearchQuery,
    ) -> Result<Vec<SearchResult>> {
        let settings = self.store.get_settings().await?.ai;
        if !settings.semantic_index_enabled {
            return Err(AppError::Unsupported(
                "semantic search is disabled; enable the semantic index in settings".to_owned(),
            ));
        }
        let Some(embedder) = self.embedder() else {
            return Err(AppError::Unsupported(
                "semantic search is not available on this platform".to_owned(),
            ));
        };
        if let nagori_ai::BackendAvailability::Unavailable(reason) = embedder.availability().await {
            return Err(AppError::Unsupported(format!(
                "the embedding model is unavailable ({reason:?})"
            )));
        }

        // Only rank against vectors the *current* model produced. If the stored
        // index metadata is incompatible (model / revision / dimension changed,
        // or the worker has not rebuilt yet after a battery pause / restart),
        // comparing a current-model query vector against old vectors of the same
        // dimension would silently mix embedding spaces. Return no results until
        // the background worker rebuilds, rather than ranking garbage.
        let current_meta = self.current_semantic_meta(embedder.as_ref()).await?;
        let stored_meta = self.store.semantic_meta().await?;
        if stored_meta.is_none_or(|stored| !stored.is_compatible_with(&current_meta)) {
            return Ok(Vec::new());
        }

        let normalized = if query.normalized.is_empty() {
            normalize_text(&query.raw)
        } else {
            query.normalized.clone()
        };
        // One embedding at a time across the whole process: share the registry's
        // embedding permit with the background indexer.
        let _permit = self.embedding_semaphore().acquire_owned().await.ok();
        let vectors = embedder
            .embed_batch(
                vec![EmbeddingInput {
                    id: "query".to_owned(),
                    text: normalized,
                }],
                CancellationToken::new(),
            )
            .await
            .map_err(|err| AppError::Ai(err.message))?;
        let Some(query_vector) = vectors.into_iter().next() else {
            return Ok(Vec::new());
        };
        self.store
            .semantic_search(query_vector.vector, query.filters, query.limit)
            .await
    }

    /// Runs the background semantic-index worker until `shutdown` fires.
    ///
    /// Spawned alongside the capture / maintenance loops. The loop wakes on a
    /// capture, a settings change, a rebuild request, or the idle tick, runs one
    /// indexing pass (subject to the guards), then sleeps again.
    pub async fn run_semantic_indexer(self, mut shutdown: ShutdownHandle) {
        let Some(embedder) = self.embedder() else {
            self.semantic.set_state(SemanticIndexState::Unsupported);
            return;
        };
        // A token that mirrors `shutdown` so an in-flight pass — the embedding
        // batch, the semaphore wait, and the backoff sleep — is interrupted
        // promptly instead of running to the Swift-side 20s timeout per item.
        let cancel = CancellationToken::new();
        {
            let cancel = cancel.clone();
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                shutdown.cancelled().await;
                cancel.cancel();
            });
        }
        let mut settings_rx = self.settings_subscribe();
        loop {
            let ai = settings_rx.borrow().ai.clone();
            if ai.semantic_index_enabled {
                self.semantic_index_pass(embedder.as_ref(), &ai, &cancel)
                    .await;
            } else {
                self.semantic.set_state(SemanticIndexState::Disabled);
            }

            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = settings_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = self.semantic.wake.notified() => {}
                () = tokio::time::sleep(IDLE_TICK) => {}
            }
        }
    }

    /// One indexing pass: reconcile model metadata, then drain pending entries
    /// in batches subject to the battery / rate-limit guards.
    async fn semantic_index_pass(
        &self,
        embedder: &dyn Embedder,
        ai: &nagori_core::AiSettings,
        cancel: &CancellationToken,
    ) {
        if let nagori_ai::BackendAvailability::Unavailable(_) = embedder.availability().await {
            self.semantic.set_state(SemanticIndexState::Unavailable);
            return;
        }
        if !self.semantic.power_allows(ai.semantic_index_ac_power_only) {
            self.semantic.set_state(SemanticIndexState::Paused);
            return;
        }
        if let Err(err) = self.reconcile_semantic_metadata(embedder).await {
            tracing::warn!(error = %err, "semantic_metadata_reconcile_failed");
            self.semantic.set_state(SemanticIndexState::Unavailable);
            return;
        }

        self.semantic.set_state(SemanticIndexState::Indexing);
        let mut backoff = 0_usize;
        loop {
            if cancel.is_cancelled() || !self.semantic.power_allows(ai.semantic_index_ac_power_only)
            {
                self.semantic.set_state(SemanticIndexState::Paused);
                return;
            }
            let pending = match self.store.semantic_pending(EMBED_BATCH).await {
                Ok(pending) => pending,
                Err(err) => {
                    tracing::warn!(error = %err, "semantic_pending_failed");
                    self.semantic.set_state(SemanticIndexState::Unavailable);
                    return;
                }
            };
            if pending.is_empty() {
                break;
            }
            match self.embed_and_store(embedder, &pending, cancel).await {
                Ok(()) => backoff = 0,
                Err(err) if err.is_transient => {
                    let wait = BACKOFF_STEPS_MS[backoff.min(BACKOFF_STEPS_MS.len() - 1)];
                    backoff += 1;
                    tracing::debug!(error = %err.detail, wait_ms = wait, "semantic_embed_backoff");
                    // Race the backoff against cancellation so shutdown is not
                    // held up by the (growing) retry delay.
                    tokio::select! {
                        () = cancel.cancelled() => {
                            self.semantic.set_state(SemanticIndexState::Paused);
                            return;
                        }
                        () = tokio::time::sleep(Duration::from_millis(wait)) => {}
                    }
                }
                Err(_) if cancel.is_cancelled() => {
                    // A batch interrupted by shutdown is not a failure.
                    self.semantic.set_state(SemanticIndexState::Paused);
                    return;
                }
                Err(err) => {
                    tracing::warn!(error = %err.detail, "semantic_embed_failed");
                    self.semantic.set_state(SemanticIndexState::Unavailable);
                    return;
                }
            }
        }
        self.semantic.set_state(SemanticIndexState::Ready);
    }

    /// The current embedder's metadata as a [`SemanticIndexMeta`].
    async fn current_semantic_meta(&self, embedder: &dyn Embedder) -> Result<SemanticIndexMeta> {
        let meta = embedder
            .metadata()
            .await
            .map_err(|err| AppError::Ai(err.message))?;
        Ok(SemanticIndexMeta {
            model_identifier: meta.model_identifier,
            revision: meta.revision,
            dimension: u32::try_from(meta.dimension).unwrap_or(0),
            max_sequence_length: u32::try_from(meta.max_sequence_length).unwrap_or(0),
            languages: meta.languages,
            index_version: INDEX_VERSION,
        })
    }

    /// Compares the live embedder's metadata against the persisted index
    /// metadata; on a mismatch (or an explicit rebuild request) clears the
    /// stored vectors and records the new metadata so the pass rebuilds.
    async fn reconcile_semantic_metadata(&self, embedder: &dyn Embedder) -> Result<()> {
        let current = self.current_semantic_meta(embedder).await?;
        // Read (don't yet clear) the rebuild flag: if the clear / set below
        // fails, the flag must stay set so the next pass retries instead of
        // silently dropping the user's rebuild request.
        let rebuild = self.semantic.rebuild_requested.load(Ordering::SeqCst);
        let stored = self.store.semantic_meta().await?;
        let incompatible = stored
            .as_ref()
            .is_none_or(|s| !s.is_compatible_with(&current));
        if rebuild || incompatible {
            self.store.semantic_clear().await?;
            self.store.semantic_set_meta(current).await?;
        }
        // Only now that the rebuild has actually happened do we clear the flag.
        if rebuild {
            self.semantic
                .rebuild_requested
                .store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Embeds one batch of pending entries and stores the vectors.
    async fn embed_and_store(
        &self,
        embedder: &dyn Embedder,
        pending: &[nagori_storage::PendingEmbedding],
        cancel: &CancellationToken,
    ) -> std::result::Result<(), EmbedBatchError> {
        let inputs: Vec<EmbeddingInput> = pending
            .iter()
            .map(|entry| EmbeddingInput {
                id: entry.entry_id.to_string(),
                text: entry.text.clone(),
            })
            .collect();
        // Race the permit wait against cancellation so shutdown is not blocked
        // behind an in-flight embedding holding the single permit.
        let _permit = tokio::select! {
            permit = self.embedding_semaphore().acquire_owned() => permit.ok(),
            () = cancel.cancelled() => {
                return Err(EmbedBatchError {
                    detail: "cancelled".to_owned(),
                    is_transient: false,
                });
            }
        };
        let vectors = embedder
            .embed_batch(inputs, cancel.clone())
            .await
            .map_err(EmbedBatchError::from_ai)?;
        for (entry, vector) in pending.iter().zip(vectors) {
            // Guard against the embedder returning out-of-order ids.
            let entry_id = vector.id.parse::<EntryId>().unwrap_or(entry.entry_id);
            if let Err(err) = self
                .store
                .semantic_upsert(entry_id, entry.content_hash.clone(), vector.vector)
                .await
            {
                return Err(EmbedBatchError {
                    detail: err.to_string(),
                    is_transient: false,
                });
            }
        }
        Ok(())
    }
}

/// A failed embedding batch, tagged with whether retrying is worthwhile.
struct EmbedBatchError {
    detail: String,
    is_transient: bool,
}

impl EmbedBatchError {
    fn from_ai(err: nagori_core::AiError) -> Self {
        use nagori_core::AiErrorCode;
        let is_transient = matches!(
            err.code,
            AiErrorCode::RateLimited | AiErrorCode::Timeout | AiErrorCode::BackendInternal
        );
        Self {
            detail: err.message,
            is_transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn power_guard_runs_when_disabled() {
        let state = SemanticState::new(None);
        // AC-only off → always allowed regardless of probe.
        assert!(state.power_allows(false));
    }

    #[test]
    fn power_guard_runs_on_unknown_power() {
        let state = SemanticState::new(Some(Arc::new(|| None)));
        // Unknown power errs toward running.
        assert!(state.power_allows(true));
    }

    #[test]
    fn power_guard_pauses_on_battery() {
        let battery = SemanticState::new(Some(Arc::new(|| Some(false))));
        assert!(!battery.power_allows(true));
        let ac = SemanticState::new(Some(Arc::new(|| Some(true))));
        assert!(ac.power_allows(true));
    }

    #[test]
    fn transient_codes_are_retried() {
        use nagori_core::{AiError, AiErrorCode};
        for code in [
            AiErrorCode::RateLimited,
            AiErrorCode::Timeout,
            AiErrorCode::BackendInternal,
        ] {
            let err = EmbedBatchError::from_ai(AiError::new(code, "x"));
            assert!(err.is_transient, "{code:?} should be transient");
        }
        let fatal = EmbedBatchError::from_ai(AiError::new(AiErrorCode::Unavailable, "x"));
        assert!(!fatal.is_transient);
    }

    fn compatible_meta() -> SemanticIndexMeta {
        // Matches `MockEmbedder`'s reported metadata (id/revision/dimension)
        // plus this module's `INDEX_VERSION`.
        SemanticIndexMeta {
            model_identifier: "mock-embedder".to_owned(),
            revision: 1,
            dimension: 8,
            max_sequence_length: 256,
            languages: vec!["en".to_owned(), "ja".to_owned()],
            index_version: INDEX_VERSION,
        }
    }

    /// A semantic query must not rank against stored vectors whose model is
    /// incompatible with the current embedder (the High finding): until the
    /// worker rebuilds, it returns no results rather than mixing spaces.
    #[tokio::test]
    async fn semantic_query_gated_on_metadata_compatibility() {
        use nagori_ai::{AiEngine, EmbeddingInput, MockEmbedder};
        use nagori_core::{
            AiProviderKind, AppSettings, EntryFactory, EntryRepository, SearchMode, SearchQuery,
            SettingsRepository,
        };
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().unwrap();
        let mut settings = AppSettings::default();
        settings.ai.semantic_index_enabled = true;
        store.save_settings(settings).await.unwrap();
        let id = store
            .insert(EntryFactory::from_text("hello semantic world"))
            .await
            .unwrap();

        // Store a vector under an INCOMPATIBLE model meta (same dimension).
        store
            .semantic_set_meta(SemanticIndexMeta {
                model_identifier: "other-model".to_owned(),
                revision: 9,
                dimension: 8,
                max_sequence_length: 256,
                languages: Vec::new(),
                index_version: INDEX_VERSION,
            })
            .await
            .unwrap();
        store
            .semantic_upsert(id, "h".to_owned(), vec![1.0; 8])
            .await
            .unwrap();

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store.clone())
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        let mut query = SearchQuery::new("hello", "hello".to_owned(), 10);
        query.mode = SearchMode::Semantic;
        let results = runtime
            .semantic_search_results(query.clone())
            .await
            .unwrap();
        assert!(
            results.is_empty(),
            "incompatible stored meta must not be searched"
        );

        // Switch to the compatible model meta and re-embed the entry; now the
        // query ranks against the matching vector.
        store.semantic_set_meta(compatible_meta()).await.unwrap();
        let mock = MockEmbedder::with_dimension(8);
        let vector = mock
            .embed_batch(
                vec![EmbeddingInput {
                    id: id.to_string(),
                    text: "hello semantic world".to_owned(),
                }],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        store
            .semantic_upsert(id, "h".to_owned(), vector[0].vector.clone())
            .await
            .unwrap();

        let results = runtime.semantic_search_results(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, id);
    }
}
