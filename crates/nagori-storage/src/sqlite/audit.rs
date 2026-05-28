use async_trait::async_trait;
use nagori_core::{AuditLog, EntryId, Result};
use rusqlite::params;
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, storage_err};

impl SqliteStore {
    /// Count rows in `audit_events` matching `kind`. Exposed so adjacent
    /// crates (the daemon's maintenance loop, the desktop diagnostics
    /// surface) can assert "the right resource-limit breadcrumb was
    /// written" without needing access to the connection pool. Hidden from
    /// rustdoc because it has no usage outside of integration tests and
    /// internal observability.
    #[doc(hidden)]
    pub async fn audit_event_count(&self, kind: &str) -> Result<i64> {
        let kind = kind.to_owned();
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE event_kind = ?1",
                params![kind],
                |row| row.get(0),
            )
            .map_err(|err| storage_err(&err))
        })
        .await
    }
}

#[async_trait]
impl AuditLog for SqliteStore {
    async fn record(
        &self,
        kind: &str,
        entry_id: Option<EntryId>,
        message: Option<&str>,
    ) -> Result<()> {
        let kind = kind.to_owned();
        let message = message.map(str::to_owned);
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let conn = store.conn()?;
            conn.execute(
                "INSERT INTO audit_events (id, event_kind, entry_id, message, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    kind,
                    entry_id.map(|id| id.to_string()),
                    message,
                    now,
                ],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}
