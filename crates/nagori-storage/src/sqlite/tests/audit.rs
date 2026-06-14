use nagori_core::AuditLog;
use rusqlite::params;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use super::super::*;

#[tokio::test]
async fn purge_audit_events_older_than_drops_stale_rows_and_keeps_recent_ones() {
    // The audit log is the one table with no retention-driven reclaim path, so
    // the maintenance sweep trims it on a fixed window. Pin the cutoff
    // semantics: a row past the cutoff is removed, a fresh one survives.
    let store = SqliteStore::open_memory().unwrap();

    // A row recorded "now" through the normal path.
    store
        .record("retention_count", None, Some("deleted=1"))
        .await
        .unwrap();
    // A stale row inserted directly so we control `created_at` (record() always
    // stamps the current time).
    let stale_at = OffsetDateTime::now_utc() - Duration::days(120);
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO audit_events (id, event_kind, entry_id, message, created_at)
             VALUES (?1, 'retention_count', NULL, 'old', ?2)",
            params!["stale-event", stale_at.format(&Rfc3339).unwrap()],
        )
        .unwrap();
    }
    assert_eq!(store.audit_event_count("retention_count").await.unwrap(), 2);

    let cutoff = OffsetDateTime::now_utc() - Duration::days(90);
    let removed = store.purge_audit_events_older_than(cutoff).await.unwrap();
    assert_eq!(removed, 1, "only the row older than the cutoff is removed");
    assert_eq!(
        store.audit_event_count("retention_count").await.unwrap(),
        1,
        "the recent row survives the trim",
    );
}
