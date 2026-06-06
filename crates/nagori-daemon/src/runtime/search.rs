//! The runtime's cached search entry point and the recent-search cache handle.

use std::time::{Duration, Instant};

use nagori_core::{Result, SearchMode, SearchQuery, SearchResult, SettingsRepository};

use crate::search_cache::{CacheKey, CacheLookup, SharedSearchCache, lock_or_recover};

use super::{NagoriRuntime, ShutdownHandle, elapsed_ms};

impl NagoriRuntime {
    /// Shared handle to the recent-search cache so out-of-runtime mutators
    /// (notably the [`crate::CaptureLoop`] capture path) can invalidate stale
    /// hits when they push new entries into storage.
    pub fn search_cache_handle(&self) -> SharedSearchCache {
        self.search_cache.clone()
    }

    /// Drop every cached search result and bump the cache epoch.
    ///
    /// Mutation paths must call this both *before* and *after* the storage
    /// write: the pre-call closes the "existing hit served while the
    /// mutation is in flight" window (a concurrent `search` would otherwise
    /// return cached rows that pre-date the mutation between commit and
    /// post-invalidate), while the post-call rejects any stale
    /// [`crate::search_cache::RecentSearchCache::put_if_epoch`] from a
    /// search that started in parallel and snapshotted the older epoch.
    pub fn invalidate_search_cache(&self) {
        lock_or_recover(&self.search_cache).invalidate();
    }

    /// Run a search through the runtime so callers (Tauri, IPC, CLI) all
    /// share the same entry point. Storage-layer access stays on the inside
    /// of this facade so Tauri commands can stay thin.
    ///
    /// Empty queries and short prefix queries are served from
    /// [`crate::search_cache::RecentSearchCache`] when available; longer
    /// queries fall through to `SQLite` directly because the working set
    /// turns over too quickly for caching to help.
    pub async fn search(&self, mut query: SearchQuery) -> Result<Vec<SearchResult>> {
        let started = Instant::now();
        // Log only the mode (an enum), the cache outcome, the row count, and
        // the cost — never `query.raw`/`normalized`, which carry the typed
        // text. That keeps the search path's observability free of clipboard
        // contents while still surfacing slow or cache-missing queries.
        let mode = query.mode;
        query.recent_order = self.store.get_settings().await?.recent_order;
        // Semantic mode needs a query embedding (only available here, where the
        // embedder lives), so it routes to its own embed-then-rank path rather
        // than the text-candidate cache. An empty query falls through to the
        // normal Recent path below.
        if query.mode == SearchMode::Semantic && !query.raw.trim().is_empty() {
            let results = self.semantic_search_results(query).await?;
            log_search(mode, false, results.len(), started);
            return Ok(results);
        }
        let key = CacheKey::from_query(&query);
        // Capture the epoch we observed at miss time so the post-query `put`
        // can refuse to publish stale results when a concurrent mutation
        // (capture insert, pin toggle, retention sweep, …) called
        // `invalidate` between the SQLite read and our acquisition of the
        // lock again.
        let cached_epoch = if key.is_eligible() {
            let mut cache = lock_or_recover(&self.search_cache);
            match cache.lookup(&key) {
                CacheLookup::Hit(hit) => {
                    // Release the cache mutex before logging.
                    drop(cache);
                    log_search(mode, true, hit.len(), started);
                    return Ok(hit);
                }
                CacheLookup::Miss { epoch } => Some(epoch),
            }
        } else {
            None
        };
        let results = self.store.search(query).await?;
        if let Some(epoch) = cached_epoch {
            lock_or_recover(&self.search_cache).put_if_epoch(key, results.clone(), epoch);
        }
        log_search(mode, false, results.len(), started);
        Ok(results)
    }

    /// One-shot background pass that regenerates ngrams left stale by a
    /// generator-revision upgrade (kana folding / Han 1-grams).
    ///
    /// Pre-upgrade documents carry `ngram_index_version = 0` (the migration's
    /// column default); fresh captures are stamped current, so once this drains
    /// the backlog there is nothing left to do — there is no steady-state tick.
    /// It runs *after* the daemon is already serving, so startup is never
    /// blocked; CJK recall for not-yet-rebuilt rows is briefly incomplete and
    /// self-heals as batches land. A failed or interrupted pass simply leaves
    /// the rows stale for the next daemon start to retry.
    pub async fn run_ngram_rebuild(self, mut shutdown: ShutdownHandle) {
        // Yield the single SQLite writer lock between batches so concurrent
        // captures aren't starved by the backfill.
        const BATCH_PAUSE: Duration = Duration::from_millis(10);

        let pending = match self.store.pending_ngram_rebuild().await {
            Ok(0) => return,
            Ok(pending) => pending,
            Err(err) => {
                tracing::warn!(error = %err, "ngram_rebuild_pending_check_failed");
                return;
            }
        };
        tracing::info!(pending, "ngram_rebuild_started");

        let mut done: u64 = 0;
        loop {
            if shutdown.is_cancelled() {
                tracing::info!(done, pending, "ngram_rebuild_interrupted");
                return;
            }
            match self.store.rebuild_stale_ngrams().await {
                Ok(0) => break,
                Ok(batch) => done += u64::try_from(batch).unwrap_or(0),
                Err(err) => {
                    tracing::warn!(error = %err, done, "ngram_rebuild_batch_failed");
                    return;
                }
            }
            tokio::select! {
                () = shutdown.cancelled() => {
                    tracing::info!(done, pending, "ngram_rebuild_interrupted");
                    return;
                }
                () = tokio::time::sleep(BATCH_PAUSE) => {}
            }
        }

        // Searches cached before the backfill may have missed folded / single
        // Han hits; drop them so the next query re-runs against fresh grams.
        self.invalidate_search_cache();
        tracing::info!(done, "ngram_rebuild_completed");
    }
}

/// Emit the per-search observability event. `mode` is an enum discriminant and
/// the remaining fields are counts/timings, so this never records query text.
fn log_search(mode: SearchMode, cache_hit: bool, row_count: usize, started: Instant) {
    tracing::debug!(
        mode = ?mode,
        cache_hit,
        row_count,
        elapsed_ms = elapsed_ms(started),
        "runtime_search"
    );
}
