use nagori_core::{AppError, Result};
use rusqlite::{TransactionBehavior, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

/// Rows fetched per eviction round in [`SqliteStore::enforce_total_bytes`].
/// Each round runs in its own transaction, so this also bounds how long one
/// round holds the writer; a larger overshoot just runs more rounds.
pub(crate) const TOTAL_BYTES_EVICTION_BATCH: i64 = 64;

/// Rows hard-deleted per transaction by the bulk reclaim paths
/// ([`SqliteStore::purge_deleted`], [`SqliteStore::clear_older_than`],
/// [`SqliteStore::enforce_retention_count`]).
///
/// A hard delete cascades into representations, blobs, embeddings,
/// thumbnails, and the FTS/ngram index, and `secure_delete` zeroes every freed
/// page — trimming a 20k-row history measured ~25s. Doing that in one
/// transaction holds the single writer for the whole run, so a capture landing
/// meanwhile waits out `busy_timeout` and fails with `database is locked`, and
/// any clip copied before the capture loop's retry is lost. Committing every
/// `HARD_DELETE_BATCH` rows and returning the pooled connection between
/// batches caps the writer-lock hold at roughly one batch (~100–300ms at the
/// measured per-row cost) while the total work stays the same.
pub(super) const HARD_DELETE_BATCH: i64 = 256;

/// Pause after a full hard-delete batch (or eviction round) before the next
/// one takes the writer again.
///
/// Short batches alone do not let a capture in: `SQLite`'s busy handler is a
/// sleep-and-retry with no hand-off, so a reclaim that re-took the lock the
/// moment it committed could win every time while the capture slept, and the
/// capture would still run out its `busy_timeout`. The default busy handler's
/// back-off tops out at 100ms between retries, so a gap longer than that
/// guarantees a waiting writer retries — and gets the lock — before the
/// reclaim's next batch.
const HARD_DELETE_YIELD: std::time::Duration = std::time::Duration::from_millis(120);

/// `PRAGMA auto_vacuum` value for a database without auto-vacuum (`1` is full,
/// `2` incremental).
const AUTO_VACUUM_NONE: i64 = 0;

/// Free pages released per `incremental_vacuum` statement in
/// [`SqliteStore::vacuum`].
///
/// Each statement is its own autocommit write transaction, so the chunk bounds
/// how long a reclaim holds the single writer: 2048 pages is 8 MiB at the
/// default 4 KiB page size, so a capture that lands mid-reclaim waits for one
/// chunk instead of the whole freelist.
const INCREMENTAL_VACUUM_CHUNK_PAGES: i64 = 2048;

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
    checkpoint_truncate(conn);
}

/// Best-effort `wal_checkpoint(TRUNCATE)`: the caller's write already
/// committed, so a failed truncate is logged rather than surfaced.
///
/// A reader that outlasts `busy_timeout` does not make the pragma fail — it
/// reports `busy = 1` in its result row and leaves the WAL untruncated — so the
/// row is read and that case logged too, instead of passing silently.
fn checkpoint_truncate(conn: &rusqlite::Connection) {
    match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(0) => {}
        Ok(_) => tracing::warn!("wal_checkpoint_truncate_busy"),
        Err(err) => tracing::warn!(error = %err, "wal_checkpoint_truncate_failed"),
    }
}

impl SqliteStore {
    /// Physically delete aged-out, non-pinned rows. `ON DELETE CASCADE`
    /// (plus `recursive_triggers` firing `search_documents_ad_fts`) drops each
    /// row's representations, image/blob payloads, embeddings, thumbnails, and
    /// search/ngram index along with it, so the content is gone from the live
    /// table rather than tombstoned and left on disk indefinitely. No
    /// `deleted_at` predicate: a hard delete of an already-tombstoned row
    /// (from a per-entry delete) is just cleanup we want anyway.
    ///
    /// Runs in [`HARD_DELETE_BATCH`]-sized transactions so a large backlog
    /// (the first sweep after shortening the retention window) cannot hold the
    /// writer long enough to make a concurrent capture fail.
    pub async fn clear_older_than(&self, cutoff: OffsetDateTime) -> Result<usize> {
        let cutoff = format_time(cutoff)?;
        self.hard_delete_in_batches(move |tx| {
            tx.execute(
                "DELETE FROM entries
                 WHERE id IN (
                     SELECT id FROM entries
                     WHERE pinned = 0 AND created_at < ?1
                     LIMIT ?2
                 )",
                params![cutoff, HARD_DELETE_BATCH],
            )
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
    /// Runs in [`HARD_DELETE_BATCH`]-sized transactions, each on a freshly
    /// checked-out pooled connection, so a bulk clear's backlog cannot hold the
    /// single writer for the whole reclaim and starve captures behind it.
    pub async fn purge_deleted(&self) -> Result<usize> {
        self.hard_delete_in_batches(|tx| {
            tx.execute(
                "DELETE FROM entries
                 WHERE id IN (
                     SELECT id FROM entries WHERE deleted_at IS NOT NULL LIMIT ?1
                 )",
                params![HARD_DELETE_BATCH],
            )
        })
        .await
    }

    /// Run `delete_batch` in its own transaction until it removes no rows,
    /// then fold the WAL back once if anything was deleted.
    ///
    /// `delete_batch` must delete at most [`HARD_DELETE_BATCH`] rows and pick
    /// them from the *current* table state, so each round sees the effect of
    /// the previous one and of any capture that committed in between. Every
    /// round checks out a fresh pooled connection and a full batch is followed
    /// by [`HARD_DELETE_YIELD`], which is what lets a capture take the writer
    /// between rounds. A failing round still checkpoints the batches already
    /// committed before surfacing the error, so their deleted content does
    /// not linger in the WAL.
    async fn hard_delete_in_batches<F>(&self, delete_batch: F) -> Result<usize>
    where
        F: Fn(&rusqlite::Transaction<'_>) -> rusqlite::Result<usize> + Clone + Send + 'static,
    {
        let mut total = 0_usize;
        loop {
            let delete_batch = delete_batch.clone();
            let round = self
                .run_blocking(move |store| {
                    let mut conn = store.conn()?;
                    // `BEGIN IMMEDIATE` so a round that queues behind a
                    // capture waits on `busy_timeout` for the writer, rather
                    // than starting as a reader and failing the upgrade.
                    let tx = conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(storage_err)?;
                    let changed = delete_batch(&tx).map_err(storage_err)?;
                    tx.commit().map_err(storage_err)?;
                    Ok(changed)
                })
                .await;
            let removed = match round {
                Ok(removed) => removed,
                Err(err) => return Err(self.checkpoint_after_partial_delete(total, err).await),
            };
            if removed == 0 {
                break;
            }
            total += removed;
            if i64::try_from(removed).is_ok_and(|removed| removed >= HARD_DELETE_BATCH) {
                tokio::time::sleep(HARD_DELETE_YIELD).await;
            }
        }
        if total > 0 {
            self.checkpoint_truncate().await;
        }
        Ok(total)
    }

    /// Checkpoint the batches a bulk delete committed before `err` stopped it
    /// — the rows are gone, so their content must not survive in the WAL —
    /// then hand `err` back for the caller to surface.
    async fn checkpoint_after_partial_delete(&self, deleted: usize, err: AppError) -> AppError {
        if deleted > 0 {
            self.checkpoint_truncate().await;
        }
        err
    }

    /// Fold the WAL back into the main file once a batched delete has drained.
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
            tracing::warn!(error = %err, "batched_delete_checkpoint_failed");
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
        // Physically delete the oldest live, unpinned rows beyond the cap.
        // The cap bounds *live* history, so the subquery selects from
        // `deleted_at IS NULL` rows; the cascade drops each evicted row's
        // representations, blobs, embeddings, thumbnails, and search index
        // with it, so a retention cap actually reclaims disk instead of
        // leaving tombstones that grow the file forever. Each batch takes the
        // next rows past the cap, so a capture committed between batches
        // simply shifts one more old row over it.
        self.hard_delete_in_batches(move |tx| {
            tx.execute(
                "DELETE FROM entries
                 WHERE id IN (
                     SELECT id FROM entries
                     WHERE deleted_at IS NULL AND pinned = 0
                     ORDER BY created_at DESC
                     LIMIT ?2 OFFSET ?1
                 )",
                params![max_entries_i64, HARD_DELETE_BATCH],
            )
        })
        .await
    }

    /// Evict the oldest live, unpinned rows until their retained payload fits
    /// `max_total_bytes`.
    ///
    /// Each round of [`TOTAL_BYTES_EVICTION_BATCH`] candidates commits on its
    /// own (see [`evict_total_bytes_round`]), so shrinking the budget far below
    /// a large history cannot hold the writer for the whole eviction and make
    /// a concurrent capture fail. The budget total is re-read every round, so a
    /// capture that lands between rounds is counted against the budget like
    /// any other row.
    pub async fn enforce_total_bytes(&self, max_total_bytes: u64) -> Result<usize> {
        let mut deleted = 0;
        loop {
            let round = self
                .run_blocking(move |store| {
                    let mut conn = store.conn()?;
                    evict_total_bytes_round(&mut conn, max_total_bytes)
                })
                .await;
            let round = match round {
                Ok(round) => round,
                Err(err) => return Err(self.checkpoint_after_partial_delete(deleted, err).await),
            };
            deleted += round.removed;
            if round.done {
                break;
            }
            tokio::time::sleep(HARD_DELETE_YIELD).await;
        }
        if deleted > 0 {
            self.checkpoint_truncate().await;
        }
        Ok(deleted)
    }

    /// Return the database's free pages to the filesystem.
    ///
    /// Databases run in `auto_vacuum = INCREMENTAL` (set before the first
    /// table in `configure_connection`), so this normally releases the
    /// freelist in [`INCREMENTAL_VACUUM_CHUNK_PAGES`] chunks: no copy of the
    /// database is built and the writer is free between chunks. A database
    /// created before that mode existed is still `auto_vacuum = NONE`, where
    /// the only way to shrink the file — and to switch the mode — is a full
    /// `VACUUM`; that one-time rebuild runs with `temp_store = FILE`, see
    /// [`rebuild_as_incremental`].
    pub async fn vacuum(&self) -> Result<()> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let mode: i64 = conn
                .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
                .map_err(storage_err)?;
            // Only `NONE` needs the rebuild: `configure_connection` switches a
            // `FULL` database to `INCREMENTAL` in place (`SQLite` allows that
            // without a `VACUUM`), and a full-mode freelist is empty anyway.
            if mode == AUTO_VACUUM_NONE {
                rebuild_as_incremental(&conn)?;
            } else {
                release_free_pages(&conn)?;
            }
            // In WAL mode both paths write every page they move through the
            // WAL, so a full rebuild leaves a sidecar as large as the database
            // itself. The autocheckpoint copies those frames back but leaves
            // the file at that size, still holding pre-rewrite page images.
            // Truncate it here instead of waiting for `journal_size_limit` to
            // trim it on the next write.
            checkpoint_truncate(&conn);
            Ok(())
        })
        .await
    }
}

/// Outcome of one [`evict_total_bytes_round`].
struct EvictionRound {
    removed: usize,
    /// No further round can make progress: the budget is met, or no
    /// candidate is left to evict.
    done: bool,
}

/// One committed round of [`SqliteStore::enforce_total_bytes`].
fn evict_total_bytes_round(
    conn: &mut rusqlite::Connection,
    max_total_bytes: u64,
) -> Result<EvictionRound> {
    // `BEGIN IMMEDIATE`: the round reads the budget total and then deletes. A
    // DEFERRED transaction would upgrade its read snapshot to a write lock at
    // the first DELETE and fail with `SQLITE_BUSY_SNAPSHOT` whenever a capture
    // committed in between, skipping the sweep; taking the write lock up front
    // waits on `busy_timeout` instead.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_err)?;
    // Budget the retained representation payload only — the `content_json`
    // envelope is bookkeeping, not user content, and for text-shaped entries
    // the same text already appears in `entry_representations.text_content`.
    // Counting both would double-charge text rows and trigger over-eager
    // eviction.
    //
    // `entries.total_byte_count` is materialised by the
    // `entry_representations_ai/ad/au_total` triggers, so the budget total is
    // a single-table aggregate.
    //
    // Pinned rows are outside the budget, mirroring `enforce_retention_count`:
    // eviction can never delete them, so counting their bytes would only let
    // pins crowd out history. Once the pinned total alone exceeded the cap,
    // every sweep would evict all unpinned rows (including a clip captured
    // seconds earlier) and still end over budget.
    let total_i64: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(total_byte_count), 0)
             FROM entries
             WHERE deleted_at IS NULL AND pinned = 0",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage_err)?;
    let mut total = u64::try_from(total_i64).map_err(|err| {
        AppError::storage(format!("entry size total overflowed u64 conversion: {err}"))
    })?;
    if total <= max_total_bytes {
        tx.commit().map_err(storage_err)?;
        return Ok(EvictionRound {
            removed: 0,
            done: true,
        });
    }

    // Evict oldest-first in bounded rounds rather than loading every live,
    // unpinned row into memory up front: a 100k-row history would otherwise
    // materialise the whole id list inside the write lock. The
    // `total_byte_count DESC` tie-break keeps same-instant rows leaving
    // largest-first, so freeing the budget costs as few rows as possible;
    // SQLite holds only the LIMIT-sized top-N while sorting. Each round
    // re-selects from the live set, so rows deleted by the previous round never
    // reappear.
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
                        AppError::storage(format!("entry size overflowed u64 conversion: {err}"))
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut removed = 0usize;
    for (id, bytes) in &candidates {
        if total <= max_total_bytes {
            break;
        }
        // Hard-delete (cascade drops representations / blobs / embeddings /
        // search index) so trimming to the byte budget reclaims real disk.
        // `pinned = 0` is a defensive guard; the candidate set is already
        // unpinned and live.
        let changed = tx
            .execute(
                "DELETE FROM entries WHERE id = ?1 AND pinned = 0",
                params![id],
            )
            .map_err(storage_err)?;
        if changed > 0 {
            removed += changed;
            total = total.saturating_sub(*bytes);
        }
    }
    tx.commit().map_err(storage_err)?;
    // An empty candidate set means every unpinned row is gone — only reachable
    // when the running total drifted from the live set — so stop rather than
    // spin. A non-empty round that removed nothing cannot shrink the live set
    // either: the candidate DELETE matches every selected (live, unpinned)
    // row, so this is normally unreachable, but the guard keeps the loop from
    // depending on that invariant. It keys on *rows removed*, not bytes freed:
    // oldest-first eviction legitimately passes through zero-byte rows (an
    // entry with no retained representation payload) before reaching the
    // heavier rows that actually free the budget.
    Ok(EvictionRound {
        removed,
        done: total <= max_total_bytes || removed == 0,
    })
}

/// Drain the freelist of an `auto_vacuum = INCREMENTAL` database one chunk at
/// a time, so a capture waits at most one chunk for the writer.
fn release_free_pages(conn: &rusqlite::Connection) -> Result<()> {
    let freelist_count = |conn: &rusqlite::Connection| -> Result<i64> {
        conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .map_err(storage_err)
    };
    let mut remaining = freelist_count(conn)?;
    while remaining > 0 {
        conn.execute_batch(&format!(
            "PRAGMA incremental_vacuum({INCREMENTAL_VACUUM_CHUNK_PAGES});"
        ))
        .map_err(storage_err)?;
        let next = freelist_count(conn)?;
        // A concurrent delete can refill the freelist between chunks; keep
        // going while chunks make progress, but never spin on a count that
        // stopped falling.
        if next >= remaining {
            break;
        }
        remaining = next;
    }
    Ok(())
}

/// Rebuild an `auto_vacuum = NONE` database with `VACUUM`, switching it to
/// incremental mode so later reclaims take [`release_free_pages`].
///
/// `VACUUM` copies the whole database into a temporary database first. Under
/// the connection's usual `temp_store = MEMORY` that copy lives in RAM — the
/// peak resident set measured ~790 MB for a 600 MB database versus ~180 MB
/// with `temp_store = FILE` — so a multi-GB image history could exhaust memory.
/// The rebuild therefore switches this connection to `FILE` for the one
/// statement. `SQLite` creates that temporary file `0600` and unlinks it right
/// after opening, so the copy has no path on disk; the connection goes back to
/// `MEMORY` whether or not the rebuild succeeded.
fn rebuild_as_incremental(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA temp_store = FILE;",
    )
    .map_err(storage_err)?;
    let rebuilt = conn.execute_batch("VACUUM").map_err(storage_err);
    let restored = conn
        .execute_batch("PRAGMA temp_store = MEMORY;")
        .map_err(storage_err);
    rebuilt.and(restored)
}
