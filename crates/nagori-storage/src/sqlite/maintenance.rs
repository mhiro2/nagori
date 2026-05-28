use nagori_core::{AppError, Result};
use rusqlite::params;
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};
use super::search::prune_deleted_search_rows;

impl SqliteStore {
    pub async fn clear_older_than(&self, cutoff: OffsetDateTime) -> Result<usize> {
        self.run_blocking(move |store| {
            let cutoff = format_time(cutoff)?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries
                 SET deleted_at = ?1, updated_at = ?1
                 WHERE pinned = 0 AND deleted_at IS NULL AND created_at < ?2",
                    params![now, cutoff],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(changed)
        })
        .await
    }

    /// Soft-delete every non-pinned entry. Used by the desktop's
    /// `clear_on_quit` setting and the secondary "Clear history" hotkey.
    /// Pinned rows survive so users can keep curated snippets across the
    /// purge.
    pub async fn clear_non_pinned(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries
                 SET deleted_at = ?1, updated_at = ?1
                 WHERE pinned = 0 AND deleted_at IS NULL",
                    params![now],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
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
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    // Mirror `clear_older_than` and the per-entry delete by
                    // bumping `updated_at` alongside `deleted_at`. Without
                    // this, retention-evicted rows look like they were last
                    // touched at insert time even though the soft-delete
                    // itself is a mutation, and downstream consumers that
                    // watch `updated_at` for change detection (sync,
                    // analytics, audit replays) miss the eviction.
                    "UPDATE entries
                 SET deleted_at = ?1, updated_at = ?1
                 WHERE deleted_at IS NULL
                   AND pinned = 0
                   AND id IN (
                       SELECT id FROM entries
                       WHERE deleted_at IS NULL AND pinned = 0
                       ORDER BY created_at DESC
                       LIMIT -1 OFFSET ?2
                   )",
                    params![now, max_entries_i64],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
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

            let now = format_time(OffsetDateTime::now_utc())?;
            let mut deleted = 0;
            for (id, bytes) in candidates {
                if total <= max_total_bytes {
                    break;
                }
                let changed = tx
                    .execute(
                        "UPDATE entries SET deleted_at = ?1, updated_at = ?1
                         WHERE id = ?2 AND deleted_at IS NULL AND pinned = 0",
                        params![now, id],
                    )
                    .map_err(|err| storage_err(&err))?;
                if changed > 0 {
                    deleted += changed;
                    total = total.saturating_sub(bytes);
                }
            }
            if deleted > 0 {
                prune_deleted_search_rows(&tx)?;
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
