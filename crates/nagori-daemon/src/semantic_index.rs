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

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nagori_ai::{Embedder, EmbeddingInput};
use nagori_core::{
    AppError, AppSettings, EntryId, Result, SearchQuery, SearchResult, SemanticIndexMeta,
    SemanticIndexState, SemanticIndexStatus, Sensitivity, SensitivityClassifier,
    SettingsRepository, normalize_text,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::runtime::{NagoriRuntime, ShutdownHandle};

/// Probe returning whether the host is on AC power: `Some(true)` on AC,
/// `Some(false)` on battery, `None` if it cannot be determined.
pub type PowerProbe = std::sync::Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// Bumped when the indexing pipeline's content shaping changes in a way that
/// invalidates previously-stored vectors for the same model.
///
/// `2`: `Secret` entries are no longer embedded and `Private` bodies are
/// redacted before embedding. Vectors produced under `1` may carry raw
/// secret / private content, so the bump forces a clear + rebuild.
const INDEX_VERSION: u32 = 2;

/// Entries embedded per `embed_batch` call. Small so each batch stays
/// cancellable and a settings/power change is observed promptly between batches.
const EMBED_BATCH: usize = 16;

/// Idle re-check cadence. Covers AC-power changes, late asset downloads, and any
/// missed wake so backfill always makes progress without a capture.
const IDLE_TICK: Duration = Duration::from_mins(1);

/// How long an interactive semantic query waits for the shared embedding
/// permit before giving up. The background indexer holds the permit for a
/// whole batch (up to [`EMBED_BATCH`] items, each allowed the backend's
/// per-item timeout), so an unbounded wait could park the query — and the
/// IPC connection slot driving it — for minutes behind backfill. A palette
/// search is interactive: better to fail fast with a clear "busy" error the
/// caller can surface than to look wedged.
const QUERY_EMBED_PERMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an interactive query waits for its single embedding to come back
/// once it holds the permit. An explicit consumer-side deadline so a wedged
/// backend can't park the palette search (and its IPC connection slot) on the
/// embedding model's own internal timeout — which the Apple bridge applies
/// per item but may not always fire. Generous relative to a sub-second
/// on-device embed, tight enough that a stall surfaces as a clear error.
const QUERY_EMBED_TIMEOUT_MS: u64 = 10_000;
const QUERY_EMBED_TIMEOUT: Duration = Duration::from_millis(QUERY_EMBED_TIMEOUT_MS);

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
        // Read the enable flag from the in-memory watch, not the store: this is
        // the interactive query path, and `get_settings()` pays a SQLite
        // round-trip plus a full `validate()` (which recompiles every
        // `regex_denylist` pattern) on each call — the same cost the text
        // search path already dropped. Mirror that path's seed guard: before
        // the startup `refresh_settings_from_store` lands the watch still holds
        // `AppSettings::default()`, so fall back to a store read until it does
        // (a handful of times at most) rather than reporting "disabled" off the
        // compile-time default.
        let semantic_enabled = if self.settings_watch_seeded() {
            self.with_settings(|settings| settings.ai.semantic_index_enabled)
        } else {
            self.refresh_settings_from_store()
                .await?
                .ai
                .semantic_index_enabled
        };
        if !semantic_enabled {
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
        // embedding permit with the background indexer — but never wait for it
        // unboundedly. A closed semaphore (`Err` from `acquire_owned`) degrades
        // to running without the permit, as before.
        let _permit = tokio::time::timeout(
            QUERY_EMBED_PERMIT_TIMEOUT,
            self.embedding_semaphore().acquire_owned(),
        )
        .await
        .map_err(|_| {
            AppError::Ai(
                "semantic search timed out waiting for the embedding model \
                 (busy indexing); try again shortly"
                    .to_owned(),
            )
        })?
        .ok();
        // Bound the embed itself, not just the permit wait. Cancel the token on
        // timeout so the backend unwinds any in-flight FFI work rather than
        // outliving this request.
        let cancel = CancellationToken::new();
        let vectors = match tokio::time::timeout(
            QUERY_EMBED_TIMEOUT,
            embedder.embed_batch(
                vec![EmbeddingInput {
                    id: "query".to_owned(),
                    text: normalized,
                }],
                cancel.clone(),
                // Match the backend's wedge watchdog to this query's consumer
                // budget so it stops wasting work once this `timeout` gives up
                // and cancels, instead of running to a fixed cap.
                Some(QUERY_EMBED_TIMEOUT_MS),
            ),
        )
        .await
        {
            Ok(result) => result.map_err(|err| AppError::Ai(err.message))?,
            Err(_elapsed) => {
                cancel.cancel();
                return Err(AppError::Ai(
                    "semantic query embedding timed out".to_owned(),
                ));
            }
        };
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
            // No indexing on this host, but a macOS-origin DB opened on
            // Windows/Linux may still carry v1 vectors that the privacy
            // migration must erase. Retry until the index is clean (or
            // shutdown) rather than giving up after one attempt — the purge is
            // the only thing protecting that at-rest content here.
            self.semantic.set_state(SemanticIndexState::Unsupported);
            loop {
                if self.purge_incompatible_index_version().await {
                    return;
                }
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(IDLE_TICK) => {}
                }
            }
        };
        // A token that mirrors `shutdown` so an in-flight pass — the embedding
        // batch, the semaphore wait, and the backoff sleep — is interrupted
        // promptly instead of running to the Swift-side 20s timeout per item.
        let cancel = CancellationToken::new();
        // Bound the bridge task to this function's lifetime. Without `done` the
        // spawned task parks on `shutdown.cancelled()` until *global* shutdown,
        // so each panic-respawn of the indexer leaks another live bridge task
        // (the supervisor re-enters here with a fresh `cancel`). The drop guard
        // fires `done` when this function returns — including a panic unwind —
        // so the bridge exits with it instead of accumulating.
        let done = CancellationToken::new();
        let _done_guard = done.clone().drop_guard();
        {
            let cancel = cancel.clone();
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = shutdown.cancelled() => cancel.cancel(),
                    () = done.cancelled() => {}
                }
            });
        }
        let mut settings_rx = self.settings_subscribe();
        loop {
            // Privacy migration *before* the per-pass guards, retried on every
            // wake until it succeeds — so a transient failure is not stranded
            // until the next restart even while the index stays disabled or the
            // model is unavailable. Idempotent and cheap: once the incompatible
            // vectors are gone (or the stored version already matches) it is a
            // single singleton-row read.
            let _ = self.purge_incompatible_index_version().await;
            let settings = settings_rx.borrow().clone();
            if settings.ai.semantic_index_enabled {
                self.semantic_index_pass(embedder.as_ref(), &settings, &settings_rx, &cancel)
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
        settings: &AppSettings,
        settings_rx: &tokio::sync::watch::Receiver<AppSettings>,
        cancel: &CancellationToken,
    ) {
        let ac_power_only = settings.ai.semantic_index_ac_power_only;
        if let nagori_ai::BackendAvailability::Unavailable(_) = embedder.availability().await {
            self.semantic.set_state(SemanticIndexState::Unavailable);
            return;
        }
        if !self.semantic.power_allows(ac_power_only) {
            self.semantic.set_state(SemanticIndexState::Paused);
            return;
        }
        // Built once per pass so every `Private` body is scrubbed through the
        // settings-aware redactor (built-in detectors + the user's
        // `regex_denylist`) before it reaches the embedding model. A broken
        // `regex_denylist` fails closed — same as the capture loop — so a
        // private body is never embedded verbatim because a rule won't compile.
        let classifier = match SensitivityClassifier::try_new(settings.clone()) {
            Ok(classifier) => classifier,
            Err(err) => {
                tracing::warn!(error = %err, "semantic_classifier_build_failed");
                self.semantic.set_state(SemanticIndexState::Unavailable);
                return;
            }
        };
        if let Err(err) = self.reconcile_semantic_metadata(embedder).await {
            tracing::warn!(error = %err, "semantic_metadata_reconcile_failed");
            self.semantic.set_state(SemanticIndexState::Unavailable);
            return;
        }

        self.semantic.set_state(SemanticIndexState::Indexing);
        let mut backoff = 0_usize;
        loop {
            if cancel.is_cancelled() || !self.semantic.power_allows(ac_power_only) {
                self.semantic.set_state(SemanticIndexState::Paused);
                return;
            }
            // A settings change observed mid-backfill must take effect
            // promptly, not after this pass (a multi-hour backfill on a large
            // history) drains. `semantic_index_enabled` toggling OFF is a
            // privacy operation, and an edited `regex_denylist` changes what
            // the per-pass classifier scrubs before content reaches the
            // model. Abort to the outer loop, which re-reads the fresh
            // settings and resumes, disables, or rebuilds the classifier as
            // needed; its `settings_rx.changed()` then clears the flag so the
            // next pass runs to completion. `has_changed` errors only once
            // every sender is dropped (shutdown) — treat that as a reason to
            // stop too.
            if settings_rx.has_changed().unwrap_or(true) {
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
            match self
                .embed_and_store(embedder, &classifier, &pending, cancel)
                .await
            {
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

    /// Unconditionally drop stored vectors whose `index_version` no longer
    /// matches [`INDEX_VERSION`]. Runs *before* the enabled / availability /
    /// battery guards, because an `INDEX_VERSION` bump is a privacy migration
    /// (e.g. the Secret/Private shaping change): vectors built under the old
    /// shaping may carry content that the new policy forbids embedding, so they
    /// must be erased even if the index is currently disabled or the model is
    /// unreachable. A model-identity change is handled separately by
    /// `reconcile_semantic_metadata`, which needs the live embedder metadata
    /// this path deliberately avoids fetching.
    ///
    /// Returns whether the index is now free of incompatible-version vectors:
    /// `true` when there was nothing to purge or the purge succeeded, `false`
    /// when the probe or clear failed (so incompatible vectors may remain and
    /// the caller should retry). Idempotent — once cleared, the stored meta is
    /// gone and a later call is a single singleton-row read.
    async fn purge_incompatible_index_version(&self) -> bool {
        match self.store.semantic_meta().await {
            Ok(Some(meta)) if meta.index_version != INDEX_VERSION => {
                match self.store.semantic_clear().await {
                    Ok(()) => {
                        tracing::info!(
                            stored = meta.index_version,
                            current = INDEX_VERSION,
                            "semantic_index_version_purged",
                        );
                        true
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "semantic_index_version_purge_failed");
                        false
                    }
                }
            }
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(error = %err, "semantic_index_version_probe_failed");
                false
            }
        }
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
        classifier: &SensitivityClassifier,
        pending: &[nagori_storage::PendingEmbedding],
        cancel: &CancellationToken,
    ) -> std::result::Result<(), EmbedBatchError> {
        let inputs: Vec<EmbeddingInput> = pending
            .iter()
            .map(|entry| EmbeddingInput {
                id: entry.entry_id.to_string(),
                // `Private` bodies are scrubbed before they reach the model so
                // private content is never embedded verbatim; `Public` /
                // `Unknown` bodies embed as-is, and `Secret` never reaches here
                // (excluded by `semantic_pending`).
                text: if entry.sensitivity == Sensitivity::Private {
                    classifier.redact(&entry.text)
                } else {
                    entry.text.clone()
                },
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
            // Background indexing has no interactive deadline; leave the
            // backend's default per-item wedge backstop. A shutdown still
            // interrupts the batch promptly through `cancel`.
            .embed_batch(inputs, cancel.clone(), None)
            .await
            .map_err(EmbedBatchError::from_ai)?;

        // Reconcile the returned vectors against the requested entries by id
        // rather than trusting positional order. A backend that reorders, drops,
        // or duplicates results would otherwise pair a vector with the wrong
        // entry's `content_hash` (zip aligns by position, not id), silently
        // corrupting staleness detection. Validate the count, the id set
        // (every result requested, none duplicated), and the dimension against
        // the model's declared width *before* persisting anything, then write
        // the whole batch in one transaction so a bad batch is rejected wholesale.
        if vectors.len() != pending.len() {
            return Err(EmbedBatchError::invalid(format!(
                "embedder returned {} vectors for {} inputs",
                vectors.len(),
                pending.len()
            )));
        }
        // Check against the dimension the model declares, not just batch-internal
        // uniformity: a backend that returns the whole batch at the wrong width
        // would otherwise be stored and mismatch the model-tagged index metadata.
        let expected_dim = embedder
            .dimension()
            .await
            .map_err(EmbedBatchError::from_ai)?;
        if expected_dim == 0 {
            return Err(EmbedBatchError::invalid(
                "embedder reported a zero embedding dimension".to_owned(),
            ));
        }
        let by_id: HashMap<EntryId, &nagori_storage::PendingEmbedding> = pending
            .iter()
            .map(|entry| (entry.entry_id, entry))
            .collect();
        let mut seen = HashSet::with_capacity(pending.len());
        let mut batch = Vec::with_capacity(pending.len());
        for vector in vectors {
            let entry_id = vector.id.parse::<EntryId>().map_err(|_| {
                EmbedBatchError::invalid(format!(
                    "embedder returned an unparseable id {:?}",
                    vector.id
                ))
            })?;
            let Some(entry) = by_id.get(&entry_id) else {
                return Err(EmbedBatchError::invalid(format!(
                    "embedder returned an id ({entry_id}) that was not requested"
                )));
            };
            if !seen.insert(entry_id) {
                return Err(EmbedBatchError::invalid(format!(
                    "embedder returned a duplicate id ({entry_id})"
                )));
            }
            if vector.vector.len() != expected_dim {
                return Err(EmbedBatchError::invalid(format!(
                    "embedder returned a {}-dim vector for {entry_id}, expected {expected_dim}",
                    vector.vector.len()
                )));
            }
            batch.push((entry_id, entry.content_hash.clone(), vector.vector));
        }
        // Count matched, every id was requested, and none repeated ⇒ the
        // returned set equals the requested set, so no pending entry is skipped.
        self.store
            .semantic_upsert_batch(batch)
            .await
            .map_err(|err| EmbedBatchError {
                detail: err.to_string(),
                is_transient: false,
            })?;
        Ok(())
    }
}

/// A failed embedding batch, tagged with whether retrying is worthwhile.
struct EmbedBatchError {
    detail: String,
    is_transient: bool,
}

impl EmbedBatchError {
    /// A batch that failed validation (id mismatch, wrong count, bad
    /// dimensions). Not transient: re-requesting the same batch from a
    /// misbehaving backend would just fail the same way, so surface it rather
    /// than spin.
    const fn invalid(detail: String) -> Self {
        Self {
            detail,
            is_transient: false,
        }
    }

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

    /// The interactive query path shares the single embedding permit with the
    /// background indexer, which holds it for a whole batch (potentially
    /// minutes of model time). The query-side acquire is deadline-bounded so a
    /// search arriving while the permit is held fails fast with a clear "busy"
    /// error instead of parking the IPC handler — and its connection slot —
    /// behind backfill.
    #[tokio::test(start_paused = true)]
    async fn semantic_query_times_out_when_embedding_permit_is_held() {
        use nagori_ai::{AiEngine, MockEmbedder};
        use nagori_core::{
            AiProviderKind, AppError, AppSettings, SearchMode, SearchQuery, SettingsRepository,
        };
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().unwrap();
        let mut settings = AppSettings::default();
        settings.ai.semantic_index_enabled = true;
        store.save_settings(settings).await.unwrap();
        store.semantic_set_meta(compatible_meta()).await.unwrap();

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store)
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        // Stand in for the background indexer mid-batch.
        let _held = runtime.embedding_semaphore().acquire_owned().await.unwrap();

        let mut query = SearchQuery::new("hello", "hello".to_owned(), 10);
        query.mode = SearchMode::Semantic;
        let err = runtime
            .semantic_search_results(query)
            .await
            .expect_err("query must not wait unboundedly for the permit");
        assert!(
            matches!(err, AppError::Ai(_)),
            "busy-permit timeout should surface as an AI error, got: {err:?}"
        );
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
                None,
            )
            .await
            .unwrap();
        // Store under the entry's real content hash so the vector counts as
        // current; a stale hash would now be excluded from ranking.
        let content_hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store
            .semantic_upsert(id, content_hash, vector[0].vector.clone())
            .await
            .unwrap();

        let results = runtime.semantic_search_results(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, id);
    }

    /// Bumping `INDEX_VERSION` is a privacy migration (the Secret/Private
    /// shaping change), so vectors built under an older version must be purged
    /// from disk *unconditionally* — not gated behind the enabled / model /
    /// battery guards that protect the per-pass rebuild. Otherwise disabling
    /// the index (a privacy action) would strand vectors that may embed raw
    /// secret / private content.
    #[tokio::test]
    async fn purge_clears_vectors_built_under_an_incompatible_index_version() {
        use nagori_ai::{AiEngine, MockEmbedder};
        use nagori_core::{AiProviderKind, EntryFactory, EntryRepository};
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().unwrap();
        let id = store
            .insert(EntryFactory::from_text("indexed under the old shaping"))
            .await
            .unwrap();
        // Stored under a previous INDEX_VERSION, otherwise model-compatible.
        let mut stale = compatible_meta();
        stale.index_version = INDEX_VERSION - 1;
        store.semantic_set_meta(stale).await.unwrap();
        // Use the entry's real content hash so the vector counts as indexed
        // (a mismatching hash would read as pending re-embedding instead).
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store.semantic_upsert(id, hash, vec![1.0; 8]).await.unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store.clone())
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        assert!(
            runtime.purge_incompatible_index_version().await,
            "purge must report the index is now clean"
        );

        assert!(
            store.semantic_meta().await.unwrap().is_none(),
            "incompatible-version meta must be cleared"
        );
        assert_eq!(
            store.semantic_counts().await.unwrap().indexed,
            0,
            "incompatible-version vectors must be purged from disk"
        );
    }

    /// An embedder that returns the batch results in reverse input order,
    /// keeping each vector tagged with its own id. The indexer must pair each
    /// vector with the entry named by its id — not by position — or it would
    /// store the wrong entry's `content_hash` and silently corrupt staleness
    /// detection.
    struct ReversingEmbedder {
        dimension: usize,
    }

    #[async_trait::async_trait]
    impl Embedder for ReversingEmbedder {
        async fn availability(&self) -> nagori_ai::BackendAvailability {
            nagori_ai::BackendAvailability::Available
        }

        async fn metadata(
            &self,
        ) -> std::result::Result<nagori_ai::EmbeddingModelMetadata, nagori_core::AiError> {
            Ok(nagori_ai::EmbeddingModelMetadata {
                model_identifier: "reversing".to_owned(),
                revision: 1,
                dimension: self.dimension,
                max_sequence_length: 256,
                languages: vec!["en".to_owned()],
            })
        }

        async fn embed_batch(
            &self,
            inputs: Vec<EmbeddingInput>,
            _cancel: CancellationToken,
            _timeout_ms: Option<u64>,
        ) -> std::result::Result<Vec<nagori_ai::EmbeddingVector>, nagori_core::AiError> {
            // A distinct vector per id (first byte = a hash of the id) so a
            // mis-paired result would be observable, then reverse the order.
            let mut out: Vec<nagori_ai::EmbeddingVector> = inputs
                .into_iter()
                .map(|input| {
                    let tag = f32::from(u8::try_from(input.id.len() % 251).unwrap_or(0));
                    let mut vector = vec![0.0_f32; self.dimension];
                    vector[0] = tag;
                    nagori_ai::EmbeddingVector {
                        id: input.id,
                        vector,
                    }
                })
                .collect();
            out.reverse();
            Ok(out)
        }
    }

    /// An embedder that blocks *inside* `embed_batch` until the token is
    /// cancelled, then fails — mirroring the Apple bridge observing an
    /// in-flight cancellation and surfacing it as an `AiError`. It announces
    /// when the batch has started so the test can cancel genuinely mid-batch
    /// (after the permit is held and the embed call is in progress) rather than
    /// before it begins.
    struct BlockUntilCancelledEmbedder {
        dimension: usize,
        started: tokio::sync::mpsc::UnboundedSender<()>,
    }

    #[async_trait::async_trait]
    impl Embedder for BlockUntilCancelledEmbedder {
        async fn availability(&self) -> nagori_ai::BackendAvailability {
            nagori_ai::BackendAvailability::Available
        }

        async fn metadata(
            &self,
        ) -> std::result::Result<nagori_ai::EmbeddingModelMetadata, nagori_core::AiError> {
            Ok(nagori_ai::EmbeddingModelMetadata {
                model_identifier: "block-until-cancelled".to_owned(),
                revision: 1,
                dimension: self.dimension,
                max_sequence_length: 256,
                languages: vec!["en".to_owned()],
            })
        }

        async fn embed_batch(
            &self,
            _inputs: Vec<EmbeddingInput>,
            cancel: CancellationToken,
            _timeout_ms: Option<u64>,
        ) -> std::result::Result<Vec<nagori_ai::EmbeddingVector>, nagori_core::AiError> {
            // The batch is now in flight; let the test cancel mid-call.
            let _ = self.started.send(());
            cancel.cancelled().await;
            Err(nagori_core::AiError::new(
                nagori_core::AiErrorCode::Unknown,
                "embedding cancelled mid-batch",
            ))
        }
    }

    /// An embedder whose declared dimension disagrees with what `embed_batch`
    /// actually returns. The indexer must reject the whole batch rather than
    /// storing vectors that mismatch the model-tagged index metadata.
    struct WrongDimEmbedder {
        declared: usize,
        produced: usize,
    }

    #[async_trait::async_trait]
    impl Embedder for WrongDimEmbedder {
        async fn availability(&self) -> nagori_ai::BackendAvailability {
            nagori_ai::BackendAvailability::Available
        }

        async fn metadata(
            &self,
        ) -> std::result::Result<nagori_ai::EmbeddingModelMetadata, nagori_core::AiError> {
            Ok(nagori_ai::EmbeddingModelMetadata {
                model_identifier: "wrong-dim".to_owned(),
                revision: 1,
                dimension: self.declared,
                max_sequence_length: 256,
                languages: vec!["en".to_owned()],
            })
        }

        async fn embed_batch(
            &self,
            inputs: Vec<EmbeddingInput>,
            _cancel: CancellationToken,
            _timeout_ms: Option<u64>,
        ) -> std::result::Result<Vec<nagori_ai::EmbeddingVector>, nagori_core::AiError> {
            Ok(inputs
                .into_iter()
                .map(|input| nagori_ai::EmbeddingVector {
                    id: input.id,
                    vector: vec![0.0; self.produced],
                })
                .collect())
        }
    }

    /// A batch whose vectors do not match the model's declared dimension must be
    /// rejected wholesale, leaving nothing stored.
    #[tokio::test]
    async fn embed_and_store_rejects_wrong_dimension_batch() {
        use nagori_ai::{AiEngine, MockEmbedder};
        use nagori_core::{AiProviderKind, EntryFactory, EntryRepository};
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().unwrap();
        store
            .insert(EntryFactory::from_text("a document"))
            .await
            .unwrap();

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store.clone())
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
        let pending = store.semantic_pending(10).await.unwrap();

        let result = runtime
            .embed_and_store(
                &WrongDimEmbedder {
                    declared: 8,
                    produced: 4,
                },
                &classifier,
                &pending,
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "a dimension mismatch must be rejected");
        assert_eq!(
            store.semantic_counts().await.unwrap().indexed,
            0,
            "a rejected batch must store nothing"
        );
    }

    #[tokio::test]
    async fn embed_and_store_aborts_the_batch_on_cancellation() {
        use nagori_ai::{AiEngine, MockEmbedder};
        use nagori_core::{AiProviderKind, EntryFactory, EntryRepository};
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        // A cancellation that lands mid-pass (shutdown, or the privacy toggle
        // flipping `semantic_index_enabled` off) must abort the in-flight batch
        // at the permit gate and persist nothing — not finish embedding the
        // backlog it had already dequeued.
        let store = SqliteStore::open_memory().unwrap();
        store
            .insert(EntryFactory::from_text("a document"))
            .await
            .unwrap();

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store.clone())
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);

        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let embedder = BlockUntilCancelledEmbedder {
            dimension: 8,
            started: started_tx,
        };
        let cancel = CancellationToken::new();

        let store_fut = runtime.embed_and_store(&embedder, &classifier, &pending, &cancel);
        tokio::pin!(store_fut);

        // Drive `embed_and_store` until the embedder reports the batch is in
        // flight — proving the permit is held and the embed call has begun.
        tokio::select! {
            _ = &mut store_fut => panic!("embed_and_store returned before the batch started"),
            started = started_rx.recv() => assert!(started.is_some(), "the batch must start"),
        }

        // Cancel genuinely mid-batch; the in-flight embed must abort and the
        // whole batch must be rejected so nothing is persisted.
        cancel.cancel();
        let err = store_fut
            .await
            .expect_err("a mid-batch cancellation must not report success");
        assert!(
            !err.is_transient,
            "cancellation is terminal, not a retryable backend error",
        );
        assert_eq!(
            store.semantic_counts().await.unwrap().indexed,
            0,
            "a cancelled batch must persist nothing",
        );
    }

    /// Reordered embedder results must be matched back to their entries by id,
    /// so every entry ends up stored under *its own* content hash and none is
    /// left pending.
    #[tokio::test]
    async fn embed_and_store_matches_reordered_results_by_id() {
        use nagori_ai::{AiEngine, MockEmbedder};
        use nagori_core::{AiProviderKind, EntryFactory, EntryRepository};
        use nagori_platform::MemoryClipboard;
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().unwrap();
        store
            .insert(EntryFactory::from_text("first distinct document"))
            .await
            .unwrap();
        store
            .insert(EntryFactory::from_text("second longer distinct document"))
            .await
            .unwrap();

        let engine = AiEngine::builder(AiProviderKind::AppleNative)
            .embedder(Arc::new(MockEmbedder::with_dimension(8)))
            .build();
        let runtime = NagoriRuntime::builder(store.clone())
            .clipboard(Arc::new(MemoryClipboard::new()))
            .ai_engine(Arc::new(engine))
            .build_for_test();

        let settings = AppSettings::default();
        let classifier = SensitivityClassifier::try_new(settings).unwrap();
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2);

        let result = runtime
            .embed_and_store(
                &ReversingEmbedder { dimension: 8 },
                &classifier,
                &pending,
                &CancellationToken::new(),
            )
            .await;
        assert!(
            result.is_ok(),
            "reordered batch should store cleanly: {:?}",
            result.err().map(|err| err.detail)
        );

        // Each entry was stored under its own content hash, so none remains
        // pending and both count as indexed.
        assert!(
            store.semantic_pending(10).await.unwrap().is_empty(),
            "every entry must be stored under its own hash after a reorder"
        );
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 2);
    }
}
