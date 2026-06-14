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
            .map_err(storage_err)
        })
        .await
    }

    /// Delete audit events recorded before `cutoff`, returning the number
    /// removed.
    ///
    /// `audit_events` has no other reclaim path: every other write to the DB
    /// is bounded by retention, but the audit log would otherwise accumulate a
    /// row per privacy/retention event forever. An app whose whole purpose is
    /// erasing clipboard history should not grow an unbounded, never-pruned log
    /// table, so the maintenance sweep trims it on a fixed window. The rows
    /// carry no clipboard content (enum kind + counters), so a plain `DELETE`
    /// suffices — no WAL scrub is needed the way the entry hard-delete paths
    /// require one.
    pub async fn purge_audit_events_older_than(&self, cutoff: OffsetDateTime) -> Result<usize> {
        self.run_blocking(move |store| {
            let cutoff = format_time(cutoff)?;
            let conn = store.conn()?;
            let removed = conn
                .execute(
                    "DELETE FROM audit_events WHERE created_at < ?1",
                    params![cutoff],
                )
                .map_err(storage_err)?;
            Ok(removed)
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
            .map_err(storage_err)?;
            Ok(())
        })
        .await
    }
}
