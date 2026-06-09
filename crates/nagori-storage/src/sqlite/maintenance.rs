use nagori_core::{AppError, Result};
use rusqlite::params;
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

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
fn checkpoint_truncate_after_purge(conn: &rusqlite::Connection, deleted: usize) {
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
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
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
                .map_err(|err| storage_err(&err))?;
            tx.commit().map_err(|err| storage_err(&err))?;
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
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute("DELETE FROM entries WHERE pinned = 0", [])
                .map_err(|err| storage_err(&err))?;
            tx.commit().map_err(|err| storage_err(&err))?;
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
            AppError::Storage(format!(
                "history_retention_count {max_entries} exceeds i64 range: {err}"
            ))
        })?;
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
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
                .map_err(|err| storage_err(&err))?;
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(changed)
        })
        .await
    }

    pub async fn enforce_total_bytes(&self, max_total_bytes: u64) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
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
                .map_err(|err| storage_err(&err))?;
            let mut total = u64::try_from(total_i64).map_err(|err| {
                AppError::Storage(format!("entry size total overflowed u64 conversion: {err}"))
            })?;
            if total <= max_total_bytes {
                tx.commit().map_err(|err| storage_err(&err))?;
                return Ok(0);
            }

            let candidates = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, total_byte_count AS entry_bytes
                         FROM entries
                         WHERE deleted_at IS NULL AND pinned = 0
                         ORDER BY created_at ASC, entry_bytes DESC",
                    )
                    .map_err(|err| storage_err(&err))?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|err| storage_err(&err))?;
                let rows = rows
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|err| storage_err(&err))?;
                rows.into_iter()
                    .map(|(id, bytes)| {
                        u64::try_from(bytes)
                            .map(|bytes| (id, bytes))
                            .map_err(|err| {
                                AppError::Storage(format!(
                                    "entry size overflowed u64 conversion: {err}"
                                ))
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };

            let mut deleted = 0;
            for (id, bytes) in candidates {
                if total <= max_total_bytes {
                    break;
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
                    .map_err(|err| storage_err(&err))?;
                if changed > 0 {
                    deleted += changed;
                    total = total.saturating_sub(bytes);
                }
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(deleted)
        })
        .await
    }

    pub async fn vacuum(&self) -> Result<()> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            conn.execute_batch("VACUUM")
                .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}
