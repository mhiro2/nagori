use nagori_core::ThumbnailRecord;
use rusqlite::params;

use super::super::*;

use super::insert_text;

#[tokio::test]
async fn thumbnail_put_get_delete_roundtrip() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "host entry").await;

    let initial = store.get_thumbnail(id).await.unwrap();
    assert!(initial.is_none());

    let record = ThumbnailRecord {
        payload: vec![0xAB; 1024],
        mime_type: "image/png".to_owned(),
        width: 512,
        height: 384,
    };
    store.put_thumbnail(id, record.clone()).await.unwrap();

    let fetched = store
        .get_thumbnail(id)
        .await
        .unwrap()
        .expect("thumb present");
    assert_eq!(fetched.payload, record.payload);
    assert_eq!(fetched.mime_type, record.mime_type);
    assert_eq!(fetched.width, record.width);
    assert_eq!(fetched.height, record.height);

    let total = store.total_thumbnail_bytes().await.unwrap();
    assert_eq!(total, record.payload.len() as u64);

    store.delete_thumbnail(id).await.unwrap();
    assert!(store.get_thumbnail(id).await.unwrap().is_none());
    assert_eq!(store.total_thumbnail_bytes().await.unwrap(), 0);
}

#[tokio::test]
async fn put_thumbnail_skips_sensitive_entries() {
    // The storage write is gated to Public/Unknown rows, so a direct
    // `put_thumbnail` for a Secret / Private / Blocked entry is a silent
    // no-op — a derived image of sensitive content never lands at rest
    // even if a caller bypasses the daemon generator's gate.
    let store = SqliteStore::open_memory().unwrap();
    let record = ThumbnailRecord {
        payload: vec![0xAB; 256],
        mime_type: "image/png".to_owned(),
        width: 16,
        height: 16,
    };

    for withheld in ["secret", "private", "blocked"] {
        let id = insert_text(&store, "host entry").await;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE entries SET sensitivity = ?1 WHERE id = ?2",
                params![withheld, id.to_string()],
            )
            .unwrap();
        }
        store.put_thumbnail(id, record.clone()).await.unwrap();
        assert!(
            store.get_thumbnail(id).await.unwrap().is_none(),
            "thumbnail must not persist for a `{withheld}` entry"
        );
    }

    // Public entries still store normally (Unknown is covered by the
    // roundtrip test, which seeds via `insert_text`).
    let public_id = insert_text(&store, "public entry").await;
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE entries SET sensitivity = 'public' WHERE id = ?1",
            params![public_id.to_string()],
        )
        .unwrap();
    }
    store
        .put_thumbnail(public_id, record.clone())
        .await
        .unwrap();
    assert!(store.get_thumbnail(public_id).await.unwrap().is_some());
}

#[tokio::test]
async fn thumbnail_cascades_on_entry_purge() {
    // Soft-delete leaves the thumbnail row alone; only the final
    // `DELETE FROM entries` (e.g. via `purge_deleted`) should
    // cascade it away. Use a direct `DELETE` to simulate the purge.
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "host entry").await;
    store
        .put_thumbnail(
            id,
            ThumbnailRecord {
                payload: vec![1, 2, 3, 4],
                mime_type: "image/png".to_owned(),
                width: 16,
                height: 16,
            },
        )
        .await
        .unwrap();

    {
        let conn = store.conn().unwrap();
        conn.execute("DELETE FROM entries WHERE id = ?1", params![id.to_string()])
            .unwrap();
    }

    assert!(store.get_thumbnail(id).await.unwrap().is_none());
}

#[tokio::test]
async fn enforce_thumbnail_budget_evicts_oldest() {
    let store = SqliteStore::open_memory().unwrap();
    let id_a = insert_text(&store, "oldest").await;
    let id_b = insert_text(&store, "middle").await;
    let id_c = insert_text(&store, "newest").await;

    // Insert thumbnails in age order, advancing the recorded
    // timestamp so the LRU eviction has a deterministic ordering.
    store
        .put_thumbnail(
            id_a,
            ThumbnailRecord {
                payload: vec![0; 4_000],
                mime_type: "image/png".to_owned(),
                width: 100,
                height: 100,
            },
        )
        .await
        .unwrap();
    // Backdate id_a so the eviction ordering is unambiguous in
    // tests that run in sub-second windows.
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE entry_thumbnails SET last_accessed_at = '2000-01-01T00:00:00Z' WHERE entry_id = ?1",
            params![id_a.to_string()],
        )
        .unwrap();
    }
    store
        .put_thumbnail(
            id_b,
            ThumbnailRecord {
                payload: vec![0; 4_000],
                mime_type: "image/png".to_owned(),
                width: 100,
                height: 100,
            },
        )
        .await
        .unwrap();
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE entry_thumbnails SET last_accessed_at = '2000-01-02T00:00:00Z' WHERE entry_id = ?1",
            params![id_b.to_string()],
        )
        .unwrap();
    }
    store
        .put_thumbnail(
            id_c,
            ThumbnailRecord {
                payload: vec![0; 4_000],
                mime_type: "image/png".to_owned(),
                width: 100,
                height: 100,
            },
        )
        .await
        .unwrap();
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE entry_thumbnails SET last_accessed_at = '2000-01-03T00:00:00Z' WHERE entry_id = ?1",
            params![id_c.to_string()],
        )
        .unwrap();
    }

    // Budget of 5_000 leaves room for one row; we expect two evictions.
    let evicted = store.enforce_thumbnail_budget(5_000).await.unwrap();
    assert_eq!(evicted, 2);
    assert!(store.get_thumbnail(id_a).await.unwrap().is_none());
    assert!(store.get_thumbnail(id_b).await.unwrap().is_none());
    assert!(store.get_thumbnail(id_c).await.unwrap().is_some());
}

/// `get_thumbnail` must bump `last_accessed_at` so a hot row escapes
/// eviction even when it was generated long before its neighbours.
/// Regression for the FIFO-shaped eviction the LRU contract on
/// `enforce_thumbnail_budget` is meant to prevent.
#[tokio::test]
async fn get_thumbnail_touch_rescues_hot_row_from_eviction() {
    let store = SqliteStore::open_memory().unwrap();
    let id_a = insert_text(&store, "hot").await;
    let id_b = insert_text(&store, "cold").await;

    for id in [id_a, id_b] {
        store
            .put_thumbnail(
                id,
                ThumbnailRecord {
                    payload: vec![0; 4_000],
                    mime_type: "image/png".to_owned(),
                    width: 100,
                    height: 100,
                },
            )
            .await
            .unwrap();
    }
    // Backdate both so the in-test `get_thumbnail` touch is the only
    // recency signal that matters. Pin `created_at` to a fixed older
    // value too — the test name asserts that creation order doesn't
    // override the access-touch contract, and an explicit backdate
    // makes that intent legible from the SQL alone.
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE entry_thumbnails
                SET created_at = '1999-01-01T00:00:00Z',
                    last_accessed_at = '2000-01-01T00:00:00Z'
              WHERE entry_id = ?1",
            params![id_a.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE entry_thumbnails
                SET created_at = '1999-01-02T00:00:00Z',
                    last_accessed_at = '2000-01-02T00:00:00Z'
              WHERE entry_id = ?1",
            params![id_b.to_string()],
        )
        .unwrap();
    }
    // Touch the older row. Its `last_accessed_at` must overtake the
    // younger but un-touched row.
    let _ = store.get_thumbnail(id_a).await.unwrap();

    let evicted = store.enforce_thumbnail_budget(5_000).await.unwrap();
    assert_eq!(evicted, 1);
    assert!(
        store.get_thumbnail(id_a).await.unwrap().is_some(),
        "the touched row must survive eviction",
    );
    assert!(
        store.get_thumbnail(id_b).await.unwrap().is_none(),
        "the older-by-access row must be evicted",
    );
}
