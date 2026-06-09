//! Lazy thumbnail fetch and gated background generation.

use nagori_core::{EntryId, Result, ThumbnailRecord};

use crate::thumbnails::generate_thumbnail;

use super::NagoriRuntime;

impl NagoriRuntime {
    /// Fetch a cached thumbnail for `id`, or return `None` if the
    /// derived row has not been generated yet.
    ///
    /// Read-only — callers that want lazy generation on miss should
    /// follow this with [`Self::kick_thumbnail_generation`] and either
    /// retry the fetch on the next request (the `nagori-image://thumb/`
    /// path's `503 Retry-After`) or stream the original payload.
    pub async fn get_thumbnail(&self, id: EntryId) -> Result<Option<ThumbnailRecord>> {
        self.store.get_thumbnail(id).await
    }

    /// Kick a background thumbnail generation for `id` if one is not
    /// already in flight, returning immediately.
    ///
    /// The generator is gated by [`crate::thumbnails::ThumbnailGate`] so
    /// concurrent requests for the same entry collapse to a single decoder,
    /// and re-asserts the sensitivity check inside
    /// [`crate::thumbnails::generate_thumbnail`] as a best-effort
    /// application-layer guard — that re-read narrows the TOCTOU window so a
    /// caller bypassing the dispatch gate with a stale classification
    /// typically loses the race before `put_thumbnail` runs. The window is
    /// not closed at this layer; see `generate_thumbnail` for the
    /// storage-side invariant that would be required for a hard guarantee.
    /// Once generation completes,
    /// [`nagori_storage::SqliteStore::enforce_thumbnail_budget`] is invoked
    /// to apply the LRU sweep if the operator configured one.
    pub fn kick_thumbnail_generation(&self, id: EntryId) {
        let Some(guard) = self.thumbnail_gate.try_acquire(id) else {
            // Another request is already generating this thumbnail; the
            // first caller's `put_thumbnail` will satisfy us on the next
            // fetch.
            return;
        };
        // Admission control *before* spawning. Per-entry dedupe (the gate guard
        // above) does nothing for misses that span distinct entries — a
        // prefetch sweep or image-heavy scroll would otherwise detach a
        // `tokio::spawn` task per entry, each parked on the decode semaphore
        // and each ready to allocate hundreds of MiB. Reserving the global
        // decode slot here bounds the number of in-flight tasks to the pool
        // size; if it is saturated we drop `guard` (freeing the per-entry slot)
        // and skip the spawn.
        //
        // Trade-off: a rejected request is *not* queued, so the entry is only
        // (re)generated when something fetches it again — the preview pane's
        // 503-miss path retries once (~1s), and a re-navigation re-kicks. An
        // entry scrolled past during a burst may therefore stay un-cached until
        // revisited, falling back to streaming the original payload. That
        // fallback streams bytes without a daemon-side decode, so under the
        // very saturation that triggered the rejection it is the memory-safe
        // outcome (no 5th concurrent decode buffer) rather than a regression.
        let Some(permit) = self.thumbnail_gate.try_acquire_permit() else {
            return;
        };
        let store = self.store.clone();
        let settings_rx = self.settings_rx.clone();
        tokio::spawn(async move {
            // Hold the gate guard across the whole generation so a
            // second request that beats us to the cache lookup still
            // observes the in-flight slot, and hold the decode permit so the
            // global concurrency cap stays enforced until decode +
            // `put_thumbnail` finish.
            let _guard = guard;
            let _permit = permit;
            match generate_thumbnail(&store, id).await {
                Ok(Some(_)) => {
                    let budget = settings_rx.borrow().max_thumbnail_total_bytes;
                    if let Some(budget) = budget
                        && let Err(err) = store.enforce_thumbnail_budget(budget).await
                    {
                        tracing::warn!(error = %err, "thumbnail_budget_enforce_failed");
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(error = %err, entry_id = %id, "thumbnail_generate_failed");
                }
            }
        });
    }
}
