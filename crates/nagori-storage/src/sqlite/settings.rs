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
                .map_err(|err| storage_err(&err))?;
            let settings: AppSettings = match value {
                Some(value) => serde_json::from_str(&value).map_err(|err| json_err(&err))?,
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
            let value = serde_json::to_string_pretty(&settings).map_err(|err| json_err(&err))?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let conn = store.conn()?;
            // Every persisted write advances `revision` (the optimistic-
            // concurrency token): 1 on the first insert, +1 on each update.
            // `save_settings_checked` reads the token to reject a stale
            // full-blob overwrite, and the watch-channel broadcast carries the
            // post-write value so clients can refresh their baseline.
            conn.execute(
                "INSERT INTO settings (key, value, updated_at, revision) VALUES ('app', ?1, ?2, 1)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at,
                     revision = settings.revision + 1",
                params![value, now],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}

impl SqliteStore {
    /// Current optimistic-concurrency token for the persisted settings row,
    /// or `0` when no row exists yet (a fresh install before the first save).
    /// Paired with [`SqliteStore::save_settings_checked`] so a full-blob save
    /// can detect that the snapshot it edited is stale.
    pub async fn settings_revision(&self) -> Result<u64> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let revision: Option<i64> = conn
                .query_row(
                    "SELECT revision FROM settings WHERE key = 'app'",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| storage_err(&err))?;
            Ok(u64::try_from(revision.unwrap_or(0)).unwrap_or(0))
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
            let value = serde_json::to_string_pretty(&settings).map_err(|err| json_err(&err))?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|err| storage_err(&err))?;
            let current: i64 = tx
                .query_row("SELECT revision FROM settings WHERE key = 'app'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|err| storage_err(&err))?
                .unwrap_or(0);
            let current = u64::try_from(current).unwrap_or(0);
            if current != expected_revision {
                return Err(AppError::Conflict(format!(
                    "settings changed concurrently (expected revision {expected_revision}, found {current})"
                )));
            }
            let next = current.saturating_add(1);
            let next_i64 = i64::try_from(next).map_err(|err| {
                AppError::Storage(format!("settings revision overflowed i64: {err}"))
            })?;
            tx.execute(
                "INSERT INTO settings (key, value, updated_at, revision) VALUES ('app', ?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at,
                     revision = excluded.revision",
                params![value, now, next_i64],
            )
            .map_err(|err| storage_err(&err))?;
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(next)
        })
        .await
    }
}
