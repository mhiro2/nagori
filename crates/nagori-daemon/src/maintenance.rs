use nagori_core::{AppSettings, AuditLog, Result};
use nagori_storage::SqliteStore;
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use crate::search_cache::{SharedSearchCache, lock_or_recover};

#[derive(Clone)]
pub struct MaintenanceService {
    store: SqliteStore,
    search_cache: Option<SharedSearchCache>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub deleted_by_age: usize,
    pub deleted_by_count: usize,
    pub deleted_by_size: usize,
    pub vacuumed: bool,
}

/// Minimum number of rows that must have been deleted in a maintenance run
/// before we trigger a `VACUUM`. `SQLite` VACUUM rewrites the entire database
/// file, which is expensive and stalls writers; running it for every TTL'd
/// row burns CPU and disk for negligible space gains. Wait until the deletion
/// is large enough that reclaiming pages actually matters.
const VACUUM_DELETION_THRESHOLD: usize = 256;

impl MaintenanceService {
    pub const fn new(store: SqliteStore) -> Self {
        Self {
            store,
            search_cache: None,
        }
    }

    /// Wire a [`SharedSearchCache`] so retention sweeps that actually delete
    /// rows invalidate stale cache hits. Without it, a row evicted by
    /// `enforce_retention_age` / `enforce_retention_count` would keep
    /// surfacing in the empty-query palette until something else flushed
    /// the cache.
    #[must_use]
    pub fn with_search_cache(mut self, cache: SharedSearchCache) -> Self {
        self.search_cache = Some(cache);
        self
    }

    pub async fn run(&self, settings: &AppSettings) -> Result<MaintenanceReport> {
        // Pre-invalidate the search cache: the retention sweep is about to
        // race against any in-flight `runtime.search()`, and serving a
        // cached hit between commit and a post-only invalidate would surface
        // rows the user just told us to evict. The post-call below runs on
        // *both* success and error paths so a partial sweep — e.g.
        // `enforce_retention_count` deletes rows but `enforce_retention_age`
        // or `vacuum` then fails — still bumps the epoch and rejects any
        // stale `put_if_epoch` from a search that snapshotted the older
        // epoch.
        self.invalidate_cache();
        let result = self.run_inner(settings).await;
        self.invalidate_cache();
        result
    }

    async fn run_inner(&self, settings: &AppSettings) -> Result<MaintenanceReport> {
        // Invalidate after *each* delete step so the window between an
        // intermediate commit and the next sub-step can't leak
        // partial-mutation results into the cache. With one combined
        // post-invalidate, a search that started after the pre-call could
        // put results that reflect e.g. the count-delete but not the
        // age-delete, and a peer search could hit them before run() ends.
        let deleted_by_count = self
            .store
            .enforce_retention_count(settings.history_retention_count)
            .await?;
        if deleted_by_count > 0 {
            self.invalidate_cache();
            self.record_retention_drop("retention_count", deleted_by_count, settings)
                .await;
        }
        let deleted_by_age = self.enforce_retention_age(settings).await?;
        if deleted_by_age > 0 {
            self.invalidate_cache();
            self.record_retention_drop("retention_age", deleted_by_age, settings)
                .await;
        }
        let deleted_by_size = if let Some(max_total_bytes) = settings.max_total_bytes {
            let deleted = self.store.enforce_total_bytes(max_total_bytes).await?;
            if deleted > 0 {
                self.invalidate_cache();
                self.record_retention_drop("retention_size", deleted, settings)
                    .await;
            }
            deleted
        } else {
            0
        };
        let total_deleted = deleted_by_age + deleted_by_count + deleted_by_size;
        let vacuumed = if total_deleted >= VACUUM_DELETION_THRESHOLD {
            self.store.vacuum().await?;
            true
        } else {
            false
        };
        let report = MaintenanceReport {
            deleted_by_age,
            deleted_by_count,
            deleted_by_size,
            vacuumed,
        };
        info!(?report, "maintenance_completed");
        Ok(report)
    }

    fn invalidate_cache(&self) {
        if let Some(cache) = &self.search_cache {
            lock_or_recover(cache).invalidate();
        }
    }

    /// Best-effort audit event for a retention sweep that actually dropped
    /// rows. We deliberately log-and-swallow the error so a transient
    /// failure to write the audit row never aborts the maintenance run that
    /// already succeeded — the retention delete itself is the load-bearing
    /// side effect; the audit row is observability for support / privacy
    /// reviews. Without these events the only trace of an unexpectedly
    /// large retention sweep was a single `info!(?report)` line that
    /// disappears with log rotation.
    async fn record_retention_drop(&self, kind: &str, deleted: usize, settings: &AppSettings) {
        let detail = match kind {
            "retention_count" => format!(
                "deleted={deleted} cap={count}",
                count = settings.history_retention_count
            ),
            "retention_age" => format!(
                "deleted={deleted} days={days}",
                days = settings
                    .history_retention_days
                    .map_or_else(|| "none".to_owned(), |d| d.to_string())
            ),
            "retention_size" => format!(
                "deleted={deleted} cap_bytes={cap}",
                cap = settings
                    .max_total_bytes
                    .map_or_else(|| "none".to_owned(), |b| b.to_string())
            ),
            _ => format!("deleted={deleted}"),
        };
        if let Err(err) = self.store.record(kind, None, Some(&detail)).await {
            warn!(error = %err, kind, deleted, "audit_record_failed");
        }
    }

    pub async fn enforce_retention_age(&self, settings: &AppSettings) -> Result<usize> {
        let Some(days) = settings.history_retention_days else {
            return Ok(0);
        };
        let cutoff = OffsetDateTime::now_utc() - Duration::days(days.into());
        self.store.clear_older_than(cutoff).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nagori_core::{
        ContentKind, EntryFactory, EntryId, EntryRepository, RankReason, SearchFilters, SearchMode,
        SearchResult, Sensitivity,
    };
    use time::OffsetDateTime;

    use super::*;
    use crate::search_cache::{CacheKey, RecentSearchCache};

    async fn store_with_entries(count: usize) -> SqliteStore {
        let store = SqliteStore::open_memory().expect("memory store");
        for i in 0..count {
            let entry = EntryFactory::from_text(format!("entry {i}"));
            store.insert(entry).await.expect("insert");
        }
        store
    }

    #[tokio::test]
    async fn run_skips_vacuum_below_threshold() {
        // Even when retention deletes a couple of rows, VACUUM is too
        // expensive to run constantly. The threshold guards against the
        // capture loop turning every age-out into a full DB rewrite.
        let store = store_with_entries(2).await;
        let service = MaintenanceService::new(store);
        let settings = AppSettings {
            history_retention_count: 1,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");

        assert_eq!(report.deleted_by_count, 1);
        assert!(
            !report.vacuumed,
            "vacuum must be skipped for small deletions"
        );
    }

    #[tokio::test]
    async fn run_vacuums_when_threshold_reached() {
        // A large retention sweep should still trigger VACUUM so we actually
        // reclaim space when it's worth it.
        let store = store_with_entries(VACUUM_DELETION_THRESHOLD + 5).await;
        let service = MaintenanceService::new(store);
        let settings = AppSettings {
            history_retention_count: 1,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");

        assert!(report.deleted_by_count >= VACUUM_DELETION_THRESHOLD);
        assert!(report.vacuumed, "vacuum must run on large sweeps");
    }

    fn populated_cache() -> Arc<Mutex<RecentSearchCache>> {
        let cache = Arc::new(Mutex::new(RecentSearchCache::default()));
        cache.lock().unwrap().put(
            CacheKey {
                normalized: String::new(),
                mode: SearchMode::Auto,
                recent_order: nagori_core::RecentOrder::ByRecency,
                limit: 10,
                filters: SearchFilters::default(),
            },
            vec![SearchResult {
                entry_id: EntryId::new(),
                score: 1.0,
                rank_reason: vec![RankReason::Recent],
                preview: String::new(),
                content_kind: ContentKind::Text,
                created_at: OffsetDateTime::now_utc(),
                pinned: false,
                sensitivity: Sensitivity::Public,
                source_app_name: None,
            }],
        );
        cache
    }

    #[tokio::test]
    async fn run_invalidates_search_cache_when_rows_deleted() {
        // Retention sweeps that actually drop rows must flush the
        // empty-query / short-prefix cache; otherwise the palette would keep
        // listing entries that no longer exist on disk.
        let store = store_with_entries(2).await;
        let cache = populated_cache();
        let service = MaintenanceService::new(store).with_search_cache(cache.clone());
        let settings = AppSettings {
            history_retention_count: 1,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");

        assert!(report.deleted_by_count > 0);
        assert!(
            cache.lock().unwrap().is_empty(),
            "deleting rows must invalidate the attached search cache",
        );
    }

    #[tokio::test]
    async fn run_invalidates_cache_unconditionally() {
        // The pre-invalidate guards against the "search races a delete"
        // window, so it has to fire even when the sweep ends up deleting
        // zero rows — we can't know in advance whether the SQL DELETE will
        // match anything, and the cost of clearing a Vec is trivially
        // smaller than skipping the invalidation only to observe a stale
        // hit during a later sweep.
        let store = store_with_entries(2).await;
        let cache = populated_cache();
        let service = MaintenanceService::new(store).with_search_cache(cache.clone());
        let settings = AppSettings {
            history_retention_count: 10,
            history_retention_days: None,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");

        assert_eq!(report.deleted_by_age, 0);
        assert_eq!(report.deleted_by_count, 0);
        assert!(
            cache.lock().unwrap().is_empty(),
            "maintenance must invalidate the cache to close the search/delete race",
        );
    }

    async fn count_audit_events(store: &SqliteStore, kind: &str) -> i64 {
        store.audit_event_count(kind).await.expect("audit count")
    }

    #[tokio::test]
    async fn run_records_retention_count_drop_in_audit_log() {
        // Maintenance is the only writer of `retention_*` audit events, and
        // they're the load-bearing breadcrumbs for "where did my history
        // go?" support questions. Without the test, a refactor that
        // inadvertently moved the audit call out of the if-block would
        // ship green even though the audit table never gets a row.
        let store = store_with_entries(3).await;
        let service = MaintenanceService::new(store.clone());
        let settings = AppSettings {
            history_retention_count: 1,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");
        assert!(report.deleted_by_count > 0);

        assert_eq!(count_audit_events(&store, "retention_count").await, 1);
        assert_eq!(count_audit_events(&store, "retention_age").await, 0);
        assert_eq!(count_audit_events(&store, "retention_size").await, 0);
    }

    #[tokio::test]
    async fn run_skips_audit_when_no_rows_were_dropped() {
        // The audit row is supposed to be a notable signal — if it fired
        // every tick of the maintenance loop, the table would flood and
        // drown the genuine truncation events. Pin the contract that
        // "no deletions ⇒ no audit row" so a future contributor adding an
        // unconditional record() inside run_inner() gets a test failure.
        let store = store_with_entries(1).await;
        let service = MaintenanceService::new(store.clone());
        let settings = AppSettings {
            history_retention_count: 100,
            history_retention_days: None,
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");
        assert_eq!(report.deleted_by_count, 0);
        assert_eq!(report.deleted_by_age, 0);

        assert_eq!(count_audit_events(&store, "retention_count").await, 0);
        assert_eq!(count_audit_events(&store, "retention_age").await, 0);
        assert_eq!(count_audit_events(&store, "retention_size").await, 0);
    }

    #[tokio::test]
    async fn run_enforces_total_bytes_after_count_and_preserves_pins() {
        let store = SqliteStore::open_memory().expect("memory store");
        let pinned = EntryFactory::from_text("pinned payload ".repeat(64));
        let pinned_id = store.insert(pinned).await.expect("insert pinned");
        store.set_pinned(pinned_id, true).await.expect("pin");
        let unpinned_id = store
            .insert(EntryFactory::from_text("unpinned payload ".repeat(64)))
            .await
            .expect("insert unpinned");
        let service = MaintenanceService::new(store.clone());
        let settings = AppSettings {
            history_retention_count: 10,
            history_retention_days: None,
            max_total_bytes: Some(1),
            ..AppSettings::default()
        };

        let report = service.run(&settings).await.expect("maintenance run");

        assert_eq!(report.deleted_by_count, 0);
        assert_eq!(report.deleted_by_age, 0);
        assert_eq!(report.deleted_by_size, 1);
        assert!(store.get(pinned_id).await.unwrap().is_some());
        assert!(store.get(unpinned_id).await.unwrap().is_none());
    }
}
