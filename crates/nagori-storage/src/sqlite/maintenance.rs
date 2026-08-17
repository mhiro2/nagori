use nagori_core::{AppError, Result};
use rusqlite::{TransactionBehavior, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

/// Rows fetched per eviction round in [`SqliteStore::enforce_total_bytes`].
/// Bounds the writer transaction's working set; one round usually clears a
/// typical overshoot, and a pathological backlog just runs more rounds
/// within the same transaction.
pub(crate) const TOTAL_BYTES_EVICTION_BATCH: i64 = 64;

/// Rows hard-deleted per transaction in [`SqliteStore::purge_deleted`].
///
/// The purge cascades into representations, blobs, embeddings, thumbnails, and
/// the FTS/ngram index, and `secure_delete` zeroes every freed page — clearing
/// a 100k-row history takes tens of seconds. Doing that in one transaction
/// holds the single writer for the whole run, so a capture landing meanwhile
/// blocks behind it. Batching commits every `PURGE_DELETED_BATCH` rows and
/// returns the pooled connection between batches, which caps the writer-lock
/// hold at roughly one batch (~100ms at the measured per-row cost) while the
/// total work stays the same.
pub(super) const PURGE_DELETED_BATCH: i64 = 256;

/// Fold the WAL back into the main file and truncate it to zero length after
/// a purge that deleted at least one row.
///
/// `secure_delete = ON` zeroes the *freed pages in the main database*, but
/// the pre-deletion content also lives in the historical WAL frames written
/// before the delete; a passive `wal_autocheckpoint` neither truncates the
/// WAL nor guarantees those frames are gone. `TRUNCATE` checkpoints every
/// frame into the (now-zeroed) main file and shrinks the `-wal` sidecar to
/// zero, so the cleartext a user copied just before *Clear history* /
/// retention does not survive in `nagori.sqlite-wal`.
///
/// Best-effort: the rows are already gone once the transaction committed, so a
/// busy checkpoint (a concurrent reader holding the WAL open) must not turn a
/// successful purge into an error — clear-on-quit relies on the purge result
/// to clear its fail-closed marker. The next checkpoint or maintenance VACUUM
/// reclaims the residue instead.
///
/// `pub(super)` so the immediate hard-delete of a `Secret` row in
/// [`super::entry`]'s `mark_deleted` follows the same WAL-scrub contract as the
/// deferred purge paths here.
pub(super) fn checkpoint_truncate_after_purge(conn: &rusqlite::Connection, deleted: usize) {
    if deleted == 0 {
        return;
    }
    if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::warn!(error = %err, "wal_checkpoint_truncate_after_purge_failed");
    }
}

impl SqliteStore {
    pub async fn clear_older_than(&self, cutoff: OffsetDateTime) -> Result<usize> {
        self.run_blocking(move |store| {
            let cutoff = format_time(cutoff)?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            // Physically delete aged-out, non-pinned rows. `ON DELETE CASCADE`
            // (plus `recursive_triggers` firing `search_documents_ad_fts`)
            // drops each row's representations, image/blob payloads,
            // embeddings, thumbnails, and search/ngram index along with it, so
            // the content is gone from the live table rather than tombstoned
            // and left on disk indefinitely. No `deleted_at` predicate: a hard
            // delete of an already-tombstoned row (from a per-entry delete) is
            // just cleanup we want anyway.
            let changed = tx
                .execute(
                    "DELETE FROM entries
                     WHERE pinned = 0 AND created_at < ?1",
                    params![cutoff],
                )
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, changed);
            Ok(changed)
        })
        .await
    }

    /// Hide every non-pinned entry immediately, deferring the physical delete
    /// to [`Self::purge_deleted`]. This is the interactive "Clear history"
    /// primitive; [`Self::clear_non_pinned`] is the synchronous counterpart
    /// clear-on-quit needs.
    ///
    /// Why the split: the hard delete cascades into representations, blobs,
    /// embeddings, thumbnails, and the FTS/ngram index with `secure_delete`
    /// zeroing every freed page, which measures ~13s for 20k entries and scales
    /// from there. Nothing leaves the palette until that transaction commits,
    /// so on a large history the user clears their clipboard and keeps seeing
    /// it — the readers are still on the pre-commit snapshot. Stamping
    /// `deleted_at` instead costs ~170ms for the same 20k rows and every live
    /// query already filters `deleted_at IS NULL`, so the history disappears
    /// from the palette, CLI, and search the moment this commits.
    ///
    /// `Secret` rows are hard-deleted inline rather than tombstoned, mirroring
    /// `mark_deleted`: their cleartext must never outlive the delete, and there
    /// are few enough of them that they cannot dominate the latency.
    ///
    /// The search/ngram rows are deliberately *not* dropped here (unlike
    /// `mark_deleted`, which does drop them for the row it tombstones). A
    /// single tombstone can sit until the next 30-minute maintenance sweep, so
    /// scrubbing the index early is worth it there; a bulk clear kicks the
    /// purge immediately and the index rows are on the same cascade as the
    /// bodies, so deleting them separately would just pay the expensive part of
    /// the purge on the interactive path for no privacy gain. Every search path
    /// joins `entries` and filters `deleted_at IS NULL`, so a stale index row
    /// cannot surface a hidden entry in the meantime.
    pub async fn tombstone_non_pinned(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            // `BEGIN IMMEDIATE`: the secret purge and the tombstone must cover
            // the same row set, so take the write lock up front rather than
            // letting a capture commit between the two statements.
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let purged_secret = tx
                .execute(
                    "DELETE FROM entries
                     WHERE pinned = 0 AND deleted_at IS NULL AND sensitivity = 'secret'",
                    [],
                )
                .map_err(storage_err)?;
            let hidden = tx
                .execute(
                    "UPDATE entries SET deleted_at = ?1, updated_at = ?1
                     WHERE pinned = 0 AND deleted_at IS NULL",
                    params![now],
                )
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            // Only the secret delete freed pages; the tombstone leaves the
            // bodies in place for the purge, which runs its own checkpoint.
            checkpoint_truncate_after_purge(&conn, purged_secret);
            Ok(hidden + purged_secret)
        })
        .await
    }

    /// Physically delete every non-pinned entry. Used by the desktop's
    /// `clear_on_quit` setting, which cannot defer the reclaim: the app is
    /// exiting, so there is no later moment to run the purge in and the
    /// guarantee is that nothing survives the quit. Pinned rows survive so
    /// users can keep curated snippets across the purge.
    ///
    /// This is a *hard* delete: the cascade drops each row's representations,
    /// blobs, embeddings, thumbnails, and search/ngram index, so "Clear
    /// history" leaves nothing recoverable from the live table — including
    /// rows a per-entry delete previously tombstoned, which are unpinned and
    /// therefore swept here too.
    pub async fn clear_non_pinned(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            let changed = tx
                .execute("DELETE FROM entries WHERE pinned = 0", [])
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, changed);
            Ok(changed)
        })
        .await
    }

    /// Physically delete every soft-deleted (tombstoned) row, regardless of
    /// pin state. `mark_deleted` only tombstones — it filters the row out of
    /// every live query immediately but leaves its body, representation blobs,
    /// embeddings, thumbnails, and search/ngram index on disk so the
    /// interactive delete stays cheap. This is the deferred reclaim the
    /// maintenance loop runs: the FK cascade (plus `recursive_triggers`) drops
    /// each tombstoned row's children, so a deleted secret actually leaves the
    /// file rather than lingering indefinitely.
    ///
    /// Crucially this is the *only* path that reclaims a tombstoned **pinned**
    /// row. Every other hard-delete path is `pinned = 0` limited
    /// (`clear_older_than` / `clear_non_pinned`) or `deleted_at IS NULL`
    /// limited (`enforce_retention_count` / `enforce_total_bytes`), so a
    /// "delete this pinned secret" would otherwise keep its content, blobs,
    /// thumbnail, and embedding on disk forever — contradicting the
    /// `secure_delete` design. The `wal_checkpoint(TRUNCATE)` follow-up matches
    /// the documented purge contract so the pre-deletion cleartext cannot
    /// survive in historical WAL frames.
    ///
    /// Runs in [`PURGE_DELETED_BATCH`]-sized transactions, each on a freshly
    /// checked-out pooled connection, so a bulk clear's backlog cannot hold the
    /// single writer for the whole reclaim and starve captures behind it.
    pub async fn purge_deleted(&self) -> Result<usize> {
        let mut total = 0_usize;
        loop {
            let purged = self.purge_deleted_batch().await?;
            if purged == 0 {
                break;
            }
            total += purged;
        }
        if total > 0 {
            self.checkpoint_truncate().await;
        }
        Ok(total)
    }

    /// One batch of [`Self::purge_deleted`]. Returns the rows reclaimed, or 0
    /// when the tombstone backlog is drained.
    async fn purge_deleted_batch(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            let changed = tx
                .execute(
                    "DELETE FROM entries
                     WHERE id IN (
                         SELECT id FROM entries WHERE deleted_at IS NOT NULL LIMIT ?1
                     )",
                    params![PURGE_DELETED_BATCH],
                )
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            Ok(changed)
        })
        .await
    }

    /// Fold the WAL back into the main file once a batched purge has drained.
    /// Best-effort for the same reason as
    /// [`checkpoint_truncate_after_purge`]: the rows are already gone, so a
    /// busy checkpoint must not turn a completed purge into an error.
    async fn checkpoint_truncate(&self) {
        let checkpointed = self
            .run_blocking(move |store| {
                let conn = store.conn()?;
                checkpoint_truncate_after_purge(&conn, 1);
                Ok(())
            })
            .await;
        if let Err(err) = checkpointed {
            tracing::warn!(error = %err, "purge_deleted_checkpoint_failed");
        }
    }

    pub async fn enforce_retention_count(&self, max_entries: usize) -> Result<usize> {
        if max_entries == 0 {
            return Ok(0);
        }
        // Settings already clamps to `MAX_RETENTION_COUNT` (1_000_000), but
        // convert at the boundary so a future caller that bypasses settings
        // (FFI, manual maintenance hook) gets a clean error instead of a
        // silently truncated `OFFSET` from `as i64`.
        let max_entries_i64 = i64::try_from(max_entries).map_err(|err| {
            AppError::storage(format!(
                "history_retention_count {max_entries} exceeds i64 range: {err}"
            ))
        })?;
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            // Physically delete the oldest live, unpinned rows beyond the cap.
            // The cap bounds *live* history, so the subquery selects from
            // `deleted_at IS NULL` rows; the cascade drops each evicted row's
            // representations, blobs, embeddings, thumbnails, and search index
            // with it, so a retention cap actually reclaims disk instead of
            // leaving tombstones that grow the file forever.
            let changed = tx
                .execute(
                    "DELETE FROM entries
                     WHERE id IN (
                         SELECT id FROM entries
                         WHERE deleted_at IS NULL AND pinned = 0
                         ORDER BY created_at DESC
                         LIMIT -1 OFFSET ?1
                     )",
                    params![max_entries_i64],
                )
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, changed);
            Ok(changed)
        })
        .await
    }

    pub async fn enforce_total_bytes(&self, max_total_bytes: u64) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            // Budget the retained representation payload only — the
            // `content_json` envelope is bookkeeping, not user content, and
            // for text-shaped entries the same text already appears in
            // `entry_representations.text_content`. Counting both would
            // double-charge text rows and trigger over-eager eviction.
            //
            // `entries.total_byte_count` is materialised by the
            // `entry_representations_ai/ad/au_total` triggers, so the
            // budget total is a single-table aggregate.
            let total_i64: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(total_byte_count), 0)
                     FROM entries
                     WHERE deleted_at IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(storage_err)?;
            let mut total = u64::try_from(total_i64).map_err(|err| {
                AppError::storage(format!("entry size total overflowed u64 conversion: {err}"))
            })?;
            if total <= max_total_bytes {
                tx.commit().map_err(storage_err)?;
                return Ok(0);
            }

            // Evict oldest-first in bounded rounds rather than loading every
            // live, unpinned row into memory up front: a 100k-row history
            // would otherwise materialise the whole id list inside the write
            // lock. The `total_byte_count DESC` tie-break keeps same-instant
            // rows leaving largest-first, so freeing the budget costs as few
            // rows as before; SQLite holds only the LIMIT-sized top-N while
            // sorting, so the tie-break no longer forces the full-table Vec
            // the old implementation paid for it. Each round re-selects from
            // the live set, so rows deleted by the previous round never
            // reappear (the DELETE is in this same transaction).
            let mut deleted = 0;
            'evict: while total > max_total_bytes {
                let candidates = {
                    let mut stmt = tx
                        .prepare_cached(
                            "SELECT id, total_byte_count
                             FROM entries
                             WHERE deleted_at IS NULL AND pinned = 0
                             ORDER BY created_at ASC, total_byte_count DESC
                             LIMIT ?1",
                        )
                        .map_err(storage_err)?;
                    let rows = stmt
                        .query_map(params![TOTAL_BYTES_EVICTION_BATCH], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                        })
                        .map_err(storage_err)?;
                    let rows = rows
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(storage_err)?;
                    rows.into_iter()
                        .map(|(id, bytes)| {
                            u64::try_from(bytes)
                                .map(|bytes| (id, bytes))
                                .map_err(|err| {
                                    AppError::storage(format!(
                                        "entry size overflowed u64 conversion: {err}"
                                    ))
                                })
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                if candidates.is_empty() {
                    // Everything evictable is gone; the remainder is pinned.
                    break;
                }
                let mut removed_this_round = 0usize;
                for (id, bytes) in candidates {
                    if total <= max_total_bytes {
                        break 'evict;
                    }
                    // Hard-delete (cascade drops representations / blobs /
                    // embeddings / search index) so trimming to the byte budget
                    // reclaims real disk. `pinned = 0` is a defensive guard; the
                    // candidate set is already unpinned and live.
                    let changed = tx
                        .execute(
                            "DELETE FROM entries WHERE id = ?1 AND pinned = 0",
                            params![id],
                        )
                        .map_err(storage_err)?;
                    if changed > 0 {
                        deleted += changed;
                        removed_this_round += changed;
                        total = total.saturating_sub(bytes);
                    }
                }
                // A non-empty candidate round that removes no rows cannot shrink
                // the live set, so re-selecting would spin on the same rows.
                // The candidate DELETE matches every selected (live, unpinned)
                // row in this same transaction, so this is normally unreachable;
                // the guard removes the loop's dependence on that invariant
                // rather than trusting it implicitly. It keys on *rows removed*,
                // not bytes freed: oldest-first eviction legitimately passes
                // through zero-byte rows (an entry with no retained
                // representation payload) before reaching the heavier rows that
                // actually free the budget, so a "no bytes freed this round"
                // guard would stop eviction prematurely.
                if removed_this_round == 0 {
                    break;
                }
            }
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, deleted);
            Ok(deleted)
        })
        .await
    }

    pub async fn vacuum(&self) -> Result<()> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            conn.execute_batch("VACUUM").map_err(storage_err)?;
            Ok(())
        })
        .await
    }
}
