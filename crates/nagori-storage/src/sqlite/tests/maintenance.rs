use super::super::*;

use super::insert_text;

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
