use time::{Duration, OffsetDateTime};

use super::super::*;

use super::insert_text;

const AUTO_VACUUM_NONE: i64 = 0;
const AUTO_VACUUM_INCREMENTAL: i64 = 2;
const TEMP_STORE_MEMORY: i64 = 2;

fn pragma(store: &SqliteStore, name: &str) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

/// Fill the store and hard-delete everything so the freelist has pages for
/// `vacuum` to hand back.
async fn fill_and_clear(store: &SqliteStore) {
    for index in 0..64 {
        insert_text(
            store,
            &format!("reclaim row {index} {}", "y".repeat(16 * 1024)),
        )
        .await;
    }
    store
        .clear_older_than(OffsetDateTime::now_utc() + Duration::days(1))
        .await
        .unwrap();
    assert!(
        pragma(store, "freelist_count") > 0,
        "the clear must have freed pages for the reclaim assertions to mean anything"
    );
}

fn wal_len(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(wal).unwrap().len()
}

/// `VACUUM` in WAL mode writes the whole rebuilt file through the WAL, so
/// without a follow-up `TRUNCATE` checkpoint the sidecar stays as large as the
/// database and keeps the pre-rewrite page images.
#[tokio::test]
async fn vacuum_truncates_wal_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    for index in 0..64 {
        insert_text(
            &store,
            &format!("vacuum wal row {index} {}", "x".repeat(4096)),
        )
        .await;
    }

    store.vacuum().await.unwrap();

    assert_eq!(
        wal_len(&db_path),
        0,
        "vacuum must checkpoint-truncate the WAL sidecar"
    );
}

/// A checkpoint only rewinds the WAL; `journal_size_limit` is what shrinks the
/// file once `SQLite` resets it. One large write must not pin the sidecar at its
/// high-water mark for the life of the process.
#[tokio::test]
async fn wal_sidecar_shrinks_back_after_a_large_write() {
    const LIMIT: u64 = 16 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    {
        let conn = store.conn().unwrap();
        conn.execute_batch(
            "CREATE TABLE scratch (id INTEGER PRIMARY KEY, body BLOB);
             INSERT INTO scratch (body) VALUES (randomblob(33554432));",
        )
        .unwrap();
    }
    assert!(
        wal_len(&db_path) > LIMIT,
        "the large write must have grown the WAL past the limit for the shrink assertion to mean anything"
    );

    // The autocheckpoint after the large commit copied every frame back; the
    // next write resets the WAL and applies the size limit.
    insert_text(&store, "small write after the large one").await;

    assert!(
        wal_len(&db_path) <= LIMIT,
        "journal_size_limit must trim the WAL once it resets, got {} bytes",
        wal_len(&db_path)
    );
}

/// A new database must be created in incremental mode: the mode can only be
/// chosen before the first table, so a later `PRAGMA` alone cannot fix it.
#[tokio::test]
async fn fresh_store_uses_incremental_auto_vacuum() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(temp.path().join("nagori.sqlite")).unwrap();
    assert_eq!(pragma(&store, "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
}

#[tokio::test]
async fn vacuum_releases_the_freelist_incrementally() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    fill_and_clear(&store).await;
    let before = std::fs::metadata(&db_path).unwrap().len();

    store.vacuum().await.unwrap();

    assert_eq!(pragma(&store, "freelist_count"), 0);
    assert!(
        std::fs::metadata(&db_path).unwrap().len() < before,
        "releasing the freelist must shrink the database file"
    );
}

/// A database created before incremental mode is `auto_vacuum = NONE`; the
/// first `vacuum` rebuilds it into incremental mode (with the temporary copy
/// on disk, not in RAM) and leaves every pooled connection back on
/// `temp_store = MEMORY`.
#[tokio::test]
async fn vacuum_converts_a_database_without_auto_vacuum() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    {
        let store = SqliteStore::open(&db_path).unwrap();
        let conn = store.conn().unwrap();
        conn.execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
            .unwrap();
    }
    // Reopening runs `configure_connection` against the existing `NONE`
    // database; that must neither fail nor silently convert it.
    let store = SqliteStore::open(&db_path).unwrap();
    assert_eq!(pragma(&store, "auto_vacuum"), AUTO_VACUUM_NONE);
    fill_and_clear(&store).await;

    store.vacuum().await.unwrap();

    assert_eq!(pragma(&store, "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
    assert_eq!(pragma(&store, "freelist_count"), 0);
    let conns: Vec<_> = (0..pool::POOL_CAPACITY)
        .map(|_| store.conn().unwrap())
        .collect();
    for conn in &conns {
        let temp_store: i64 = conn
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, TEMP_STORE_MEMORY);
    }
}

/// `FULL` → `INCREMENTAL` needs no rebuild, so opening a full-mode database
/// must switch it in place and `vacuum` must never pay for a full `VACUUM`.
#[tokio::test]
async fn opening_a_full_auto_vacuum_database_switches_it_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    {
        let store = SqliteStore::open(&db_path).unwrap();
        let conn = store.conn().unwrap();
        conn.execute_batch("PRAGMA auto_vacuum = FULL; VACUUM;")
            .unwrap();
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 1, "the fixture must start in full mode");
    }

    let store = SqliteStore::open(&db_path).unwrap();

    assert_eq!(pragma(&store, "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
}

const FTS_NEEDLE: &str = "confidentialneedleabcxyz";

fn fts_index_contains(store: &SqliteStore, needle: &str) -> bool {
    let conn = store.conn().unwrap();
    let mut stmt = conn.prepare("SELECT block FROM search_fts_data").unwrap();
    let blocks = stmt
        .query_map([], |row| row.get::<_, Option<Vec<u8>>>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    blocks.into_iter().flatten().any(|block| {
        block
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    })
}

fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read(path)
        .unwrap()
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Delete one of three indexed entries and confirm its token is gone from the
/// full-text index's own storage, not merely unreachable through search.
async fn index_three_and_delete_needle(store: &SqliteStore) {
    insert_text(store, "first ordinary clipboard note").await;
    let target = insert_text(store, &format!("token {FTS_NEEDLE} inside")).await;
    insert_text(store, "third ordinary clipboard note").await;
    assert!(
        fts_index_contains(store, FTS_NEEDLE),
        "the needle must be indexed for the delete assertion to mean anything"
    );
    store.hard_delete_entry(target).await.unwrap();
}

#[tokio::test]
async fn hard_delete_removes_tokens_from_the_fts_index_and_file() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    index_three_and_delete_needle(&store).await;

    assert!(
        !fts_index_contains(&store, FTS_NEEDLE),
        "a hard delete must not leave the token in search_fts_data"
    );
    store.vacuum().await.unwrap();
    drop(store);
    assert!(
        !file_contains(&db_path, FTS_NEEDLE),
        "the deleted token must not survive in the database file"
    );
}

#[tokio::test]
async fn upgrade_purges_tokens_left_by_earlier_deletes() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    {
        let store = SqliteStore::open(&db_path).unwrap();
        // Recreate the pre-upgrade shape: FTS5 deletes without rewriting the
        // segment that holds the token.
        store
            .conn()
            .unwrap()
            .execute_batch("INSERT INTO search_fts(search_fts, rank) VALUES ('secure-delete', 0);")
            .unwrap();
        index_three_and_delete_needle(&store).await;
        assert!(
            fts_index_contains(&store, FTS_NEEDLE),
            "without secure-delete the token must linger for the upgrade to have work"
        );
        store
            .conn()
            .unwrap()
            .execute_batch("PRAGMA user_version = 105;")
            .unwrap();
    }

    let store = SqliteStore::open(&db_path).unwrap();

    assert!(
        !fts_index_contains(&store, FTS_NEEDLE),
        "the upgrade must rebuild the index without the deleted token"
    );
    let secure_delete: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT v FROM search_fts_config WHERE k = 'secure-delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(secure_delete, 1);
    // The surviving entries are still searchable after the rebuild.
    let hits: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM search_fts WHERE search_fts MATCH 'ordinary'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 2);
}
