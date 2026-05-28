use async_trait::async_trait;
use nagori_core::{AppError, AppSettings, Result, SettingsRepository, compile_user_regex};
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
        settings.validate()?;
        for pattern in &settings.regex_denylist {
            // `compile_user_regex` enforces the same DoS-resistant limits
            // (max length / nesting depth / DFA size) the in-memory
            // classifier applies, so a hostile pattern can't be persisted
            // and then triggered when the daemon next refreshes settings.
            compile_user_regex(pattern).map_err(|err| match err {
                AppError::Policy(msg) => AppError::InvalidInput(msg),
                other => other,
            })?;
        }
        self.run_blocking(move |store| {
            let value = serde_json::to_string_pretty(&settings).map_err(|err| json_err(&err))?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let conn = store.conn()?;
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ('app', ?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![value, now],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}
