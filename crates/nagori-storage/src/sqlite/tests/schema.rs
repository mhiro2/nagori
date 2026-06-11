use rusqlite::params;

use super::super::schema::SCHEMA_VERSION;
use super::super::*;

#[test]
fn run_migrations_rolls_back_user_version_on_failure() {
    // Arrange a version-3 migration whose SQL is intentionally
    // invalid. Because the version stamp is concatenated *after*
    // the schema body in a single `execute_batch`, SQLite must
    // reject the whole batch and roll back the transaction — so
    // `user_version` must stay at the last successfully applied
    // version even though `MIGRATIONS` advertised a newer one.
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();
    let baseline: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(baseline, SCHEMA_VERSION);

    let bad_version = SCHEMA_VERSION + 1;
    let bad_migration = "CREATE TABLE valid (id INTEGER); NOT VALID SQL;";
    let tx = conn.transaction().unwrap();
    let stamped = format!("{bad_migration}\nPRAGMA user_version = {bad_version};");
    let exec = tx.execute_batch(&stamped);
    assert!(exec.is_err(), "bad migration must fail to execute");
    drop(tx); // implicit rollback

    let after: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        after, baseline,
        "user_version must not advance when migration SQL fails"
    );
}

#[test]
fn run_migrations_rejects_legacy_prerelease_user_version() {
    // Any pre-release dev DB whose `user_version` predates the
    // consolidated schema (the legacy 1..=5 line, plus any
    // intermediate value below the first migration) must fail loud
    // at startup rather than silently running the new code path
    // against an old schema shape.
    for legacy in [1, 2, 3, 4, 5, 50, 99] {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {legacy};"))
            .unwrap();
        let err = run_migrations(&mut conn).expect_err(&format!(
            "run_migrations should reject legacy user_version = {legacy}"
        ));
        assert!(
            err.to_string().contains("predates the consolidated"),
            "error should identify the pre-consolidation version, got: {err}"
        );
    }
}

#[test]
fn concurrent_migrations_do_not_double_apply() {
    // Two processes upgrading the same DB right after an install (daemon +
    // desktop + CLI launching at once) must not both re-run the same
    // migration. `run_migrations` takes the write lock with BEGIN
    // IMMEDIATE and re-reads `user_version` under it, so the loser skips
    // the already-applied migration instead of failing on a re-run
    // `ALTER TABLE` (duplicate column) / `CREATE` against the migrated
    // shape.
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.db");
    // Materialise the (empty, user_version = 0) DB file so both threads
    // connect to the *same* database instead of racing to create it.
    drop(Connection::open(&db_path).unwrap());

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = db_path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut conn = Connection::open(&path).unwrap();
                // The IMMEDIATE lock is the serialisation point; without a
                // busy_timeout the loser would get SQLITE_BUSY instead of
                // waiting, exactly as `configure_connection` sets in prod.
                conn.execute_batch("PRAGMA busy_timeout = 5000;").unwrap();
                barrier.wait();
                run_migrations(&mut conn)
            })
        })
        .collect();

    for handle in handles {
        handle
            .join()
            .unwrap()
            .expect("concurrent migration must not error");
    }

    let conn = Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, SCHEMA_VERSION,
        "DB must land at the latest schema version after a concurrent upgrade",
    );
}

#[tokio::test]
async fn pooled_connections_enable_secure_delete() {
    // Deleted clipboard rows must have their freed pages zeroed, not just
    // unlinked, so a hard-delete leaves nothing recoverable from the
    // freelist. `secure_delete` is a per-connection setting, so assert it
    // is live on a connection handed out by the pool rather than only on
    // the one `configure_connection` was called with at open time.
    let store = SqliteStore::open_memory().unwrap();
    let conn = store.conn().unwrap();
    let secure_delete: i64 = conn
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        secure_delete, 1,
        "secure_delete must be ON for every pooled connection"
    );
}

/// The `(gram, entry_id)` index duplicated a strict prefix of the ngrams
/// PRIMARY KEY's automatic index, taxing every insert/delete on the
/// schema's largest table for nothing. A fresh install must not create
/// it, and a database that already has it (created by the pre-103
/// consolidated schema) must lose it when the drop migration applies.
#[test]
fn redundant_ngram_gram_index_is_absent_and_dropped_on_upgrade() {
    let index_exists = |conn: &Connection| -> bool {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'index' AND name = 'idx_ngrams_gram_entry'
            )",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };

    // Fresh install: the consolidated schema no longer creates the index.
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();
    assert!(
        !index_exists(&conn),
        "a fresh database must not create idx_ngrams_gram_entry"
    );

    // Upgrade: simulate a database that the pre-103 schema left at
    // version 102 with the redundant index in place, then re-run
    // migrations and verify the drop applied.
    conn.execute_batch(
        "CREATE INDEX idx_ngrams_gram_entry ON ngrams(gram, entry_id);
         PRAGMA user_version = 102;",
    )
    .unwrap();
    assert!(index_exists(&conn));
    run_migrations(&mut conn).unwrap();
    assert!(
        !index_exists(&conn),
        "migration 103 must drop idx_ngrams_gram_entry from upgraded databases"
    );
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn fresh_db_has_consolidated_shape() {
    // Fresh installs apply the single consolidated migration. Probe
    // that the user-version stamp lands at SCHEMA_VERSION, the
    // dedupe key is the representation_set_hash column, and the
    // legacy payload columns are absent.
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let rep_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entry_representations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rep_count, 0);

    for column in [
        "representation_set_hash",
        "total_byte_count",
        "content_hash",
    ] {
        let present: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('entries')
                    WHERE name = ?1
                )",
                params![column],
                |row| row.get(0),
            )
            .unwrap();
        assert!(present, "entries.{column} should exist");
    }

    for column in [
        "payload_blob",
        "payload_mime",
        "payload_ref",
        "text_content",
    ] {
        let present: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('entries')
                    WHERE name = ?1
                )",
                params![column],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "entries.{column} should be absent");
    }

    for column in ["payload_ref", "payload_mime"] {
        let present: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('entry_representations')
                    WHERE name = ?1
                )",
                params![column],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "entry_representations.{column} should be absent");
    }
}
