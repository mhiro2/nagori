use rusqlite::params;

use super::super::schema::{self, SCHEMA_VERSION};
use super::super::*;

#[test]
fn failed_migration_rolls_back_its_ddl_data_and_version_stamp() {
    // Run the real migration driver over the shipped list plus two
    // appended steps: one that succeeds and one that creates a table,
    // writes a row and then hits invalid SQL. The successful step must
    // stay committed at its own version, while the failing step must
    // leave neither its DDL, its row, nor its `user_version` stamp.
    let mut conn = Connection::open_in_memory().unwrap();
    let good_version = SCHEMA_VERSION + 1;
    let bad_version = SCHEMA_VERSION + 2;
    let mut migrations = schema::MIGRATIONS.to_vec();
    migrations.push((good_version, "CREATE TABLE applied_step (id INTEGER);"));
    migrations.push((
        bad_version,
        "CREATE TABLE half_applied (id INTEGER);
         INSERT INTO half_applied (id) VALUES (1);
         INSERT INTO applied_step (id) VALUES (1);
         NOT VALID SQL;",
    ));

    let err = schema::apply_migrations(&mut conn, &migrations)
        .expect_err("the invalid migration must surface as an error");
    assert!(
        matches!(err, AppError::Storage { .. }),
        "migration failure must map to a storage error, got: {err}",
    );

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, good_version,
        "user_version must stop at the last migration that committed",
    );
    let table_exists = |name: &str| -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![name],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(
        table_exists("applied_step"),
        "the earlier migration must stay committed"
    );
    assert!(
        !table_exists("half_applied"),
        "DDL from the failed migration must be rolled back",
    );
    let leaked_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM applied_step", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        leaked_rows, 0,
        "data written by the failed migration must be rolled back",
    );

    // Re-running with the bad step fixed resumes from the committed
    // version instead of re-applying the earlier step.
    migrations.pop();
    migrations.push((bad_version, "CREATE TABLE half_applied (id INTEGER);"));
    schema::apply_migrations(&mut conn, &migrations).expect("fixed migration applies");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, bad_version);
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

#[test]
fn every_pooled_connection_enables_secure_delete() {
    // Deleted clipboard rows must have their freed pages zeroed, not just
    // unlinked, so a hard-delete leaves nothing recoverable from the
    // freelist. `secure_delete` is a per-connection setting, so open a
    // file-backed store (whose pool holds several physical connections)
    // and hold every slot at once: acquiring them one at a time would just
    // hand back the same idle connection.
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(temp.path().join("nagori.db")).unwrap();
    let held: Vec<_> = (0..POOL_CAPACITY).map(|_| store.conn().unwrap()).collect();
    for (slot, conn) in held.iter().enumerate() {
        let secure_delete: i64 = conn
            .query_row("PRAGMA secure_delete", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            secure_delete, 1,
            "secure_delete must be ON for pooled connection #{slot}",
        );
    }
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

/// The FTS update trigger must be scoped to the content columns with `OF`
/// so a metadata-only stamp (`rebuild_stale_ngrams` writing
/// `ngram_index_version`) does not rebuild the FTS row. A fresh install must
/// create the scoped trigger, and a database left with the old bare
/// `AFTER UPDATE` trigger (pre-104) must be recreated on upgrade.
#[test]
fn fts_update_trigger_is_scoped_and_recreated_on_upgrade() {
    let trigger_sql = |conn: &Connection| -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'search_documents_au_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };

    // Fresh install: the consolidated schema creates the scoped trigger.
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();
    assert!(
        trigger_sql(&conn).contains("UPDATE OF title, preview, normalized_text"),
        "a fresh database must scope search_documents_au_fts with OF"
    );

    // Upgrade: simulate a database the pre-104 schema left at version 103
    // with the unscoped trigger, then re-run migrations and verify the
    // scoped trigger replaced it.
    conn.execute_batch(
        "DROP TRIGGER search_documents_au_fts;
         CREATE TRIGGER search_documents_au_fts
         AFTER UPDATE ON search_documents
         BEGIN
             INSERT INTO search_fts(search_fts, rowid, title, preview, normalized_text)
             VALUES ('delete', OLD.doc_id, OLD.title, OLD.preview, OLD.normalized_text);
             INSERT INTO search_fts(rowid, title, preview, normalized_text)
             VALUES (NEW.doc_id, NEW.title, NEW.preview, NEW.normalized_text);
         END;
         PRAGMA user_version = 103;",
    )
    .unwrap();
    assert!(
        !trigger_sql(&conn).contains("UPDATE OF"),
        "test setup must install the unscoped trigger"
    );
    run_migrations(&mut conn).unwrap();
    assert!(
        trigger_sql(&conn).contains("UPDATE OF title, preview, normalized_text"),
        "migration 104 must recreate the scoped trigger on upgraded databases"
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
