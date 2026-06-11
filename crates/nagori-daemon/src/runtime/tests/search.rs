use nagori_core::{SearchQuery, SettingsRepository};

use super::super::*;
use super::runtime_with_memory_clipboard;

#[tokio::test]
async fn search_before_watch_seed_reads_persisted_recent_order() {
    // The settings watch starts at `AppSettings::default()` until the startup
    // refresh lands. A search racing that window must not serve the default
    // order: it refreshes the watch from the store itself, after which the
    // fast path takes over.
    let (runtime, _) = runtime_with_memory_clipboard();
    let persisted = AppSettings {
        recent_order: nagori_core::RecentOrder::ByUseCount,
        ..Default::default()
    };
    // Write straight to the store so the runtime's publish path never runs —
    // exactly the pre-seed state a freshly built runtime is in.
    runtime
        .store()
        .save_settings(persisted)
        .await
        .expect("settings should persist");
    assert!(!runtime.settings_watch_seeded());

    runtime
        .search(SearchQuery::new("", String::new(), 5))
        .await
        .expect("search should succeed");

    // The fallback read seeded the watch with the persisted value, so later
    // searches use the snapshot.
    assert!(runtime.settings_watch_seeded());
    assert_eq!(
        runtime.current_settings().recent_order,
        nagori_core::RecentOrder::ByUseCount
    );
}

#[tokio::test]
async fn search_cache_serves_repeat_empty_query_without_round_tripping_storage() {
    // Empty query is the hottest path (palette open). The runtime must
    // serve the repeat call from the in-memory cache so SQLite isn't
    // touched once per keystroke.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .add_text("alpha".to_owned())
        .await
        .expect("seed entry");

    let first = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("first search");
    assert_eq!(first.len(), 1);
    assert_eq!(
        runtime.search_cache_handle().lock().unwrap().len(),
        1,
        "first search should populate the cache"
    );

    let second = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("repeat search");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].entry_id, first[0].entry_id);
}

#[tokio::test]
async fn search_cache_invalidates_after_add_text() {
    // Invariant: any insert through the runtime must drop cached hits so
    // the next search reflects the new row. Without invalidation a freshly
    // captured clip wouldn't surface in the palette until the cache
    // happened to be flushed by some other mutation.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime.add_text("alpha".to_owned()).await.expect("seed");
    let _ = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("warm cache");
    assert_eq!(runtime.search_cache_handle().lock().unwrap().len(), 1);

    runtime
        .add_text("beta".to_owned())
        .await
        .expect("second entry");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "add_text must invalidate the search cache",
    );

    let results = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("post-insert search");
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_cache_invalidates_after_pin_toggle() {
    // `recent_entries` hoists pinned rows above plain ones, so toggling
    // the pin bit reorders the empty-query result. Stale cache hits would
    // hide the pin until something else cleared the cache.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("alpha".to_owned())
        .await
        .expect("seed entry");
    let _ = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("warm cache");

    runtime
        .pin_entry(id, true)
        .await
        .expect("pin should succeed");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "pin_entry must invalidate the search cache",
    );
}

#[tokio::test]
async fn search_cache_skips_long_queries() {
    // Long queries turn over too quickly to be worth caching, and would
    // crowd the small LRU. Verify we don't cache anything for a query
    // longer than `CACHEABLE_QUERY_LEN`.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .add_text("alphabetagamma".to_owned())
        .await
        .expect("seed");
    let long = "alphabetagamma".to_owned();
    let _ = runtime
        .search(SearchQuery::new(long.clone(), long, 10))
        .await
        .expect("search");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "queries longer than the cache threshold must not populate the cache",
    );
}
