use nagori_core::{AppError, Result};
use rusqlite::params;
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

/// Rows fetched per eviction round in [`SqliteStore::enforce_total_bytes`].
/// Bounds the writer transaction's working set; one round usually clears a
/// typical overshoot, and a pathological backlog just runs more rounds
/// within the same transaction.
pub(crate) const TOTAL_BYTES_EVICTION_BATCH: i64 = 64;

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

    /// Physically delete every non-pinned entry. Used by the desktop's
    /// `clear_on_quit` setting and the "Clear history" hotkey/tray action.
    /// Pinned rows survive so users can keep curated snippets across the
    /// purge.
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
    pub async fn purge_deleted(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            let changed = tx
                .execute("DELETE FROM entries WHERE deleted_at IS NOT NULL", [])
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, changed);
            Ok(changed)
        })
        .await
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
