use nagori_core::{AppError, EntryId, Result, ThumbnailRecord};
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

impl SqliteStore {
    /// Fetch a previously stored thumbnail for `id`.
    ///
    /// On hit, this also bumps `last_accessed_at` so the LRU eviction
    /// path (`enforce_thumbnail_budget`) keeps frequently-previewed
    /// rows around even when they were generated long ago. Soft-deleted
    /// entries still resolve here because the row is removed via
    /// `ON DELETE CASCADE` when the entry is finally purged, not on
    /// soft-delete — but the desktop's `nagori-image://thumb/<id>`
    /// handler re-checks sensitivity through `get_entry` before serving
    /// the bytes, so a recently-Secret-tagged entry cannot leak its
    /// pre-classification thumbnail.
    pub async fn get_thumbnail(&self, id: EntryId) -> Result<Option<ThumbnailRecord>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let record = conn
                .query_row(
                    "SELECT payload_blob, mime_type, width, height
                     FROM entry_thumbnails
                     WHERE entry_id = ?1",
                    params![id.to_string()],
                    |row| {
                        Ok(ThumbnailRecord {
                            payload: row.get::<_, Vec<u8>>(0)?,
                            mime_type: row.get::<_, String>(1)?,
                            width: row.get::<_, u32>(2)?,
                            height: row.get::<_, u32>(3)?,
                        })
                    },
                )
                .optional()
                .map_err(|err| storage_err(&err))?;
            if record.is_some() {
                let now = format_time(OffsetDateTime::now_utc())?;
                conn.execute(
                    "UPDATE entry_thumbnails SET last_accessed_at = ?1 WHERE entry_id = ?2",
                    params![now, id.to_string()],
                )
                .map_err(|err| storage_err(&err))?;
            }
            Ok(record)
        })
        .await
    }

    /// Persist a thumbnail for `id`, replacing any existing row.
    ///
    /// The caller is responsible for sensitivity gating (Public only) and
    /// for clamping the byte count before this call — the storage layer
    /// trusts what it is asked to write.
    pub async fn put_thumbnail(&self, id: EntryId, record: ThumbnailRecord) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let byte_count = i64::try_from(record.payload.len()).map_err(|err| {
                AppError::Storage(format!(
                    "thumbnail byte_count overflowed i64 conversion: {err}"
                ))
            })?;
            let conn = store.conn()?;
            conn.execute(
                "INSERT OR REPLACE INTO entry_thumbnails (
                    entry_id, payload_blob, mime_type,
                    width, height, byte_count, created_at, last_accessed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.to_string(),
                    record.payload,
                    record.mime_type,
                    record.width,
                    record.height,
                    byte_count,
                    now,
                ],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }

    /// Drop the stored thumbnail for `id` if one exists.
    pub async fn delete_thumbnail(&self, id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.execute(
                "DELETE FROM entry_thumbnails WHERE entry_id = ?1",
                params![id.to_string()],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }

    /// Total bytes currently held in `entry_thumbnails`.
    ///
    /// Surfaced via `nagori doctor` so operators can see how much of the
    /// thumbnail budget is in use, and consulted by `enforce_thumbnail_budget`
    /// to decide whether to evict.
    pub async fn total_thumbnail_bytes(&self) -> Result<u64> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let total: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(byte_count), 0) FROM entry_thumbnails",
                    [],
                    |row| row.get(0),
                )
                .map_err(|err| storage_err(&err))?;
            u64::try_from(total).map_err(|err| {
                AppError::Storage(format!(
                    "thumbnail size total overflowed u64 conversion: {err}"
                ))
            })
        })
        .await
    }

    /// Evict the least-recently-accessed thumbnails until the total
    /// thumbnail byte count is at or below `budget`.
    ///
    /// Recency is the `last_accessed_at` column, which `get_thumbnail`
    /// bumps on every cache hit. A long-running session that keeps
    /// previewing the same row therefore won't see it evicted by
    /// newer-but-untouched neighbours. Returns the number of rows
    /// evicted. The deletion is unconditional on the entry's pin state
    /// — thumbnails are pure derived data and are transparently
    /// regenerable on the next preview request.
    pub async fn enforce_thumbnail_budget(&self, budget: u64) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let total_i64: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(byte_count), 0) FROM entry_thumbnails",
                    [],
                    |row| row.get(0),
                )
                .map_err(|err| storage_err(&err))?;
            let mut total = u64::try_from(total_i64).map_err(|err| {
                AppError::Storage(format!(
                    "thumbnail size total overflowed u64 conversion: {err}"
                ))
            })?;
            if total <= budget {
                tx.commit().map_err(|err| storage_err(&err))?;
                return Ok(0);
            }
            let candidates = {
                let mut stmt = tx
                    .prepare(
                        "SELECT entry_id, byte_count
                         FROM entry_thumbnails
                         ORDER BY last_accessed_at ASC",
                    )
                    .map_err(|err| storage_err(&err))?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|err| storage_err(&err))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|err| storage_err(&err))?
            };
            let mut evicted = 0usize;
            for (entry_id, bytes) in candidates {
                if total <= budget {
                    break;
                }
                let changed = tx
                    .execute(
                        "DELETE FROM entry_thumbnails WHERE entry_id = ?1",
                        params![entry_id],
                    )
                    .map_err(|err| storage_err(&err))?;
                if changed > 0 {
                    evicted += changed;
                    let bytes = u64::try_from(bytes).unwrap_or(0);
                    total = total.saturating_sub(bytes);
                }
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(evicted)
        })
        .await
    }
}
