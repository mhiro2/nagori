use async_trait::async_trait;
use nagori_core::{AppError, AppSettings, Result, SettingsRepository};
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, json_err, storage_err};

#[async_trait]
impl SettingsRepository for SqliteStore {
    async fn get_settings(&self) -> Result<AppSettings> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let value: Option<String> = conn
                .query_row("SELECT value FROM settings WHERE key = 'app'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage_err)?;
            let settings: AppSettings = match value {
                Some(value) => serde_json::from_str(&value).map_err(json_err)?,
                None => return Ok(AppSettings::default()),
            };
            // Hand-edited or downgraded rows can carry out-of-range values
            // (`paste_delay_ms = u64::MAX`, `palette_row_count = 0`, …) that
            // wedge the consumer. Validate on every load — the same gate
            // `save_settings` enforces — so a corrupt row surfaces loudly
            // instead of silently freezing paste or breaking the palette.
            settings.validate()?;
            Ok(settings)
        })
        .await
    }

    async fn save_settings(&self, settings: AppSettings) -> Result<()> {
        // `validate` also compiles every `regex_denylist` pattern under the
        // same DoS-resistant limits (max length / nesting depth / DFA size)
        // the in-memory classifier applies, so a hostile pattern can't be
        // persisted and then triggered when the daemon next refreshes
        // settings.
        settings.validate()?;
        self.run_blocking(move |store| {
            let value = serde_json::to_string_pretty(&settings).map_err(json_err)?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let purged = invalidate_semantic_index_on_policy_change(&tx, &settings)?;
            // Every persisted write advances `revision` (the optimistic-
            // concurrency token): 1 on the first insert, +1 on each update.
            // `save_settings_checked` reads the token to reject a stale
            // full-blob overwrite, and the watch-channel broadcast carries the
            // post-write value so clients can refresh their baseline.
            tx.execute(
                "INSERT INTO settings (key, value, updated_at, revision) VALUES ('app', ?1, ?2, 1)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at,
                     revision = settings.revision + 1",
                params![value, now],
            )
            .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            super::maintenance::checkpoint_truncate_after_purge(&conn, purged);
            Ok(())
        })
        .await
    }
}

/// Erase the semantic index — vectors, exclusion tombstones, and the metadata
/// row — in the *same transaction* as a settings write whose embedding policy
/// fingerprint differs from the stored one.
///
/// This is what makes a policy edit atomic against the semantic index. The
/// background worker also purges on a fingerprint mismatch, but it acts
/// *after* the settings commit: without this, an indexing batch shaped by the
/// old policy could commit its vectors after the settings landed, and a
/// semantic query could rank old-policy vectors after validating against the
/// not-yet-updated metadata. Deleting the index here means any snapshot is
/// consistent — it either predates the settings write (old policy fully in
/// force) or sees no vectors at all — and `semantic_upsert_batch`'s
/// policy-hash guard makes a racing batch abort instead of re-inserting.
///
/// Returns the number of vectors erased so the caller can apply the
/// hard-delete WAL-scrub contract after commit. A missing settings row
/// fingerprints as the compile-time default (the state an index built before
/// the first explicit save ran under); an unreadable one fails closed and
/// purges.
fn invalidate_semantic_index_on_policy_change(
    tx: &rusqlite::Transaction<'_>,
    new_settings: &AppSettings,
) -> Result<usize> {
    let old_hash = stored_settings_policy_hash(tx)?;
    if old_hash.as_deref() == Some(&new_settings.semantic_policy_hash()) {
        return Ok(0);
    }
    let purged = tx
        .execute("DELETE FROM entry_embeddings", [])
        .map_err(storage_err)?;
    tx.execute("DELETE FROM semantic_exclusions", [])
        .map_err(storage_err)?;
    tx.execute("DELETE FROM semantic_index_meta", [])
        .map_err(storage_err)?;
    Ok(purged)
}

/// The semantic policy fingerprint of the settings row as committed, read
/// inside the caller's transaction so it describes the same snapshot as any
/// writes the caller is about to make.
///
/// A missing row fingerprints as the compile-time default (the state an index
/// built before the first explicit save ran under); `None` means the stored
/// row no longer deserializes — the policy it encodes cannot be
/// reconstructed, so callers must fail closed.
pub(super) fn stored_settings_policy_hash(conn: &rusqlite::Connection) -> Result<Option<String>> {
    let value: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'app'", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(storage_err)?;
    Ok(match value {
        Some(value) => serde_json::from_str::<AppSettings>(&value)
            .ok()
            .map(|old| old.semantic_policy_hash()),
        None => Some(AppSettings::default().semantic_policy_hash()),
    })
}

impl SqliteStore {
    /// Read the persisted settings *and* their optimistic-concurrency token as
    /// a single consistent pair, returning `(AppSettings::default(), 0)` when
    /// no row exists yet.
    ///
    /// Both values come from one `SELECT value, revision`, so the body and the
    /// revision always describe the same committed state. A caller that read
    /// the body and the revision in two separate statements could observe body
    /// N with revision N+1 if a write landed in between — the stale body would
    /// then pass the compare-and-swap and revert the concurrent change. This is
    /// the read side of [`SqliteStore::save_settings_checked`].
    pub async fn get_settings_with_revision(&self) -> Result<(AppSettings, u64)> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let row: Option<(String, i64)> = conn
                .query_row(
                    "SELECT value, revision FROM settings WHERE key = 'app'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(storage_err)?;
            let Some((value, revision)) = row else {
                return Ok((AppSettings::default(), 0));
            };
            let settings: AppSettings = serde_json::from_str(&value).map_err(json_err)?;
            // Mirror `get_settings`: a hand-edited or downgraded row surfaces
            // loudly here too instead of wedging the consumer.
            settings.validate()?;
            Ok((settings, u64::try_from(revision).unwrap_or(0)))
        })
        .await
    }

    /// Persist `settings` only when the stored revision still equals
    /// `expected_revision`, returning the post-write revision. A mismatch
    /// surfaces as [`AppError::Conflict`] without writing, so a client holding
    /// a stale snapshot cannot silently revert a concurrent change.
    ///
    /// The read-check-write runs in a single `IMMEDIATE` transaction so the
    /// compare and the bump are atomic against another connection in the pool.
    pub async fn save_settings_checked(
        &self,
        settings: AppSettings,
        expected_revision: u64,
    ) -> Result<u64> {
        settings.validate()?;
        self.run_blocking(move |store| {
            let value = serde_json::to_string_pretty(&settings).map_err(json_err)?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let current: i64 = tx
                .query_row("SELECT revision FROM settings WHERE key = 'app'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage_err)?
                .unwrap_or(0);
            let current = u64::try_from(current).unwrap_or(0);
            if current != expected_revision {
                return Err(AppError::Conflict(format!(
                    "settings changed concurrently (expected revision {expected_revision}, found {current})"
                )));
            }
            let purged = invalidate_semantic_index_on_policy_change(&tx, &settings)?;
            let next = current.saturating_add(1);
            let next_i64 = i64::try_from(next).map_err(|err| {
                AppError::storage(format!("settings revision overflowed i64: {err}"))
            })?;
            tx.execute(
                "INSERT INTO settings (key, value, updated_at, revision) VALUES ('app', ?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at,
                     revision = excluded.revision",
                params![value, now, next_i64],
            )
            .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            super::maintenance::checkpoint_truncate_after_purge(&conn, purged);
            Ok(next)
        })
        .await
    }
}
