mod audit;
mod convert;
mod entry;
mod maintenance;
mod permissions;
mod pool;
mod schema;
mod search;
#[cfg(feature = "semantic-index")]
mod semantic;
mod settings;
mod thumbnail;

use std::{
    path::Path,
    sync::{Arc, Condvar, Mutex},
};

use nagori_core::{AppError, Result};
use rusqlite::Connection;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub use permissions::ensure_private_directory;
#[cfg(feature = "semantic-index")]
pub use semantic::{PendingEmbedding, SemanticIndexCounts};

use convert::storage_err;
// Pulled in here so the `tests` submodule can reach the helper through
// `use super::*;` without depending on the `convert` path.
#[cfg(test)]
use convert::fts_query;
use permissions::{harden_db_file_permissions, pre_create_db_file_private};
use pool::{ConnPool, POOL_CAPACITY, PooledConn};
use schema::run_migrations;

/// Concurrent search fan-outs may each hold up to three pooled connections
/// (substring + FTS + ngram). Capping how many connections search can occupy
/// at [`SEARCH_ADMISSION_PERMITS`] keeps at least one of the [`POOL_CAPACITY`]
/// connections free for capture / maintenance writes, so a burst of (possibly
/// superseded) searches can't starve the writer. [`SqliteStore::run_search_blocking`]
/// holds the permit *inside* the blocking closure until the pooled connection
/// is returned, so the reservation holds even for a superseded fan-out whose
/// future was dropped — its progress-handler-aborted query releases the permit
/// and the connection together.
const SEARCH_ADMISSION_PERMITS: usize = POOL_CAPACITY - 1;

#[derive(Clone)]
pub struct SqliteStore {
    pool: Arc<ConnPool>,
    /// Bounded admission for search blocking work — see
    /// [`SEARCH_ADMISSION_PERMITS`]. Shared across clones so every handle to
    /// one store draws from the same budget.
    search_admission: Arc<Semaphore>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // Register sqlite-vec's scalar functions before opening any connection
        // so the semantic index can rank embeddings via `vec_distance_cosine`.
        #[cfg(feature = "semantic-index")]
        semantic::register_vec_extension();
        let path = path.as_ref();
        // Atomically create the DB file with `0600` *before* SQLite opens
        // it, closing the TOCTOU window where `Connection::open` would
        // otherwise create the file under the process umask (typically
        // world-readable) and only get tightened to `0600` afterwards. A
        // peer process running as the same user could read clipboard
        // history during that window; pre-creating with the right mode
        // means SQLite never sees a permissive file.
        pre_create_db_file_private(path)?;
        let mut primary = Connection::open(path).map_err(|err| storage_err(&err))?;
        configure_connection(&primary)?;
        // Defensive post-open tighten: covers the WAL/SHM sidecars that
        // `PRAGMA journal_mode = WAL` just created under the process
        // umask, plus re-asserts `0600` on the main file in case the
        // pre-create path saw an existing file we don't fully trust.
        harden_db_file_permissions(path)?;
        // Run migrations on the primary connection before populating the
        // rest of the pool. Otherwise additional connections opening in
        // parallel could observe a partially-migrated schema.
        run_migrations(&mut primary)?;
        let mut slots = Vec::with_capacity(POOL_CAPACITY);
        slots.push(primary);
        for _ in 1..POOL_CAPACITY {
            let conn = Connection::open(path).map_err(|err| storage_err(&err))?;
            configure_connection(&conn)?;
            slots.push(conn);
        }
        Ok(Self {
            pool: Arc::new(ConnPool {
                slots: Mutex::new(slots),
                available: Condvar::new(),
            }),
            search_admission: Arc::new(Semaphore::new(SEARCH_ADMISSION_PERMITS)),
        })
    }

    pub fn open_memory() -> Result<Self> {
        #[cfg(feature = "semantic-index")]
        semantic::register_vec_extension();
        // `Connection::open_in_memory` is a brand-new database per call, so
        // there's no way to share state across multiple in-memory
        // connections without enabling shared-cache + a named URI. For
        // tests we keep the pool at capacity 1 — equivalent to the prior
        // single-`Mutex<Connection>` behaviour.
        let mut conn = Connection::open_in_memory().map_err(|err| storage_err(&err))?;
        configure_connection(&conn)?;
        run_migrations(&mut conn)?;
        Ok(Self {
            pool: Arc::new(ConnPool {
                slots: Mutex::new(vec![conn]),
                available: Condvar::new(),
            }),
            search_admission: Arc::new(Semaphore::new(SEARCH_ADMISSION_PERMITS)),
        })
    }

    pub(crate) fn conn(&self) -> Result<PooledConn<'_>> {
        self.pool.acquire()
    }

    /// Execute `f` on tokio's blocking pool with a cloned `SqliteStore`.
    ///
    /// All `SQLite` work in this crate goes through here so the rusqlite mutex
    /// never blocks tokio's worker threads — which was the root cause of
    /// stalled IPC responses on the daemon under search load.
    pub(super) async fn run_blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Self) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || f(store))
            .await
            .map_err(|err| AppError::Storage(format!("blocking task failed: {err}")))?
    }

    /// Like [`Self::run_blocking`], but for the read-only search candidate
    /// fetches: it bounds how much of the pool search can occupy and makes the
    /// query *cancellable* mid-execution.
    ///
    /// Two cooperating mechanisms back the search-cancellation finding:
    ///
    /// * **Bounded admission** — the admission permit
    ///   ([`SEARCH_ADMISSION_PERMITS`]) is held *inside* the blocking closure
    ///   until the pooled connection is returned, so even a superseded fan-out
    ///   whose future was dropped keeps its reservation until its detached query
    ///   actually finishes. That keeps the "≥ 1 connection reserved for capture
    ///   / maintenance writes" bound honest rather than freeing the permit the
    ///   instant the future drops while a detached query still holds a
    ///   connection.
    /// * **Real cancellation** — a `progress_handler` aborts the query when
    ///   `cancel` fires (the search future was dropped — a superseded keystroke
    ///   or a sibling branch failing the `try_join`). `SQLite` invokes it
    ///   throughout statement execution, so a cancellation that lands *after*
    ///   the statement starts still stops it — the window an after-the-fact
    ///   `sqlite3_interrupt` would miss. Without it the dropped future's
    ///   `spawn_blocking` would run the LIKE / FTS / ngram query to completion,
    ///   pinning a connection long after the result is wanted. An RAII guard
    ///   removes the handler *before* the connection returns to the pool — even
    ///   on a panic — so a later borrower of the recycled connection is never
    ///   aborted by this call's (now-cancelled) token.
    pub(super) async fn run_search_blocking<F, T>(
        &self,
        cancel: &CancellationToken,
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .search_admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Storage("search admission semaphore closed".to_owned()))?;

        let store = self.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            // Declared first so it drops *last*: drop order is the reverse of
            // declaration (`_progress`, then `conn`, then `_permit`), so the
            // admission permit is released only after the pooled connection has
            // been returned.
            let _permit = permit;
            let conn = store.conn()?;
            // Removed before `conn` returns to the pool (and before `_permit`
            // releases), even if `f` panics — see [`ProgressGuard`].
            let _progress = ProgressGuard::install(&conn, cancel)?;
            f(&conn)
        })
        .await
        .map_err(|err| AppError::Storage(format!("blocking task failed: {err}")))?
    }
}

/// Virtual-machine instructions `SQLite` evaluates between successive
/// progress-handler invocations on a search query. Small enough that a
/// cancelled search aborts within a sub-millisecond of CPU work, large enough
/// that the per-call overhead on a normal query stays negligible.
const SEARCH_PROGRESS_OPS: std::ffi::c_int = 1024;

/// Installs a cancellation-driven `progress_handler` on a search connection and
/// removes it on drop, so the handler never outlives the borrow that owns the
/// pooled connection. Drop runs during unwinding too, so a panicking query
/// closure can't leave a stale handler that aborts the connection's next
/// borrower.
struct ProgressGuard<'a> {
    conn: &'a Connection,
}

impl<'a> ProgressGuard<'a> {
    fn install(conn: &'a Connection, cancel: CancellationToken) -> Result<Self> {
        conn.progress_handler(SEARCH_PROGRESS_OPS, Some(move || cancel.is_cancelled()))
            .map_err(|err| storage_err(&err))?;
        Ok(Self { conn })
    }
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        // `None` disables the handler so the connection's next borrower runs
        // without this call's (now-irrelevant) cancellation check.
        let _ = self.conn.progress_handler(0, None::<fn() -> bool>);
    }
}

fn configure_connection(conn: &Connection) -> Result<()> {
    // `temp_store = MEMORY` keeps SQLite scratch (sorter spill, transient
    // indices) off the on-disk temp files that would otherwise land in
    // `$TMPDIR` with default umask permissions — the DB file itself is
    // chmod 0o600, but the temp sidecar isn't, so this prevents a
    // narrow class of disclosure under multi-user macOS.
    //
    // `wal_autocheckpoint = 1000` (pages, ~4 MiB at the default 4 KiB
    // page size) bounds WAL growth on a long-running daemon. Without it
    // an idle writer can leave a multi-GiB WAL after a burst of
    // captures, which surprises users inspecting the data dir.
    //
    // `mmap_size = 64 MiB` lets read-heavy paths (substring scan, FTS
    // candidate fetch) skip the page-cache copy on macOS where mmap is
    // cheap. 64 MiB is small enough that we don't fight other tenants
    // for address space on 32-bit CI runners while still covering a
    // typical ~50k-row history.
    //
    // `recursive_triggers = ON` makes FK CASCADE deletes fire the AFTER
    // DELETE triggers on the cascaded child table. The
    // `search_documents_ad_fts` trigger relies on this so a hard
    // `DELETE FROM entries` (or any path that purges an entry row)
    // walks the FK cascade into `search_documents` and then drops the
    // matching `search_fts` row instead of leaking the FTS index entry.
    //
    // `secure_delete = ON` overwrites the freed content of every deleted
    // row with zeros instead of merely unlinking the b-tree cell. The
    // clipboard inevitably captures secrets, so a hard-delete (retention
    // sweep, *Clear history*, clear-on-quit) must leave nothing
    // recoverable from the freelist pages a later VACUUM or raw file read
    // would otherwise expose. This is a per-connection setting, so it has
    // to be set on every pooled connection. It is *not* a substitute for
    // full-disk encryption — freed disk blocks remain recoverable at the
    // filesystem layer until reused — and the explicit purge paths follow
    // up with `wal_checkpoint(TRUNCATE)` to drop the historical WAL frames
    // that still hold the pre-deletion content; see
    // `docs/security-encryption-at-rest.md`.
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA recursive_triggers = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA secure_delete = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA wal_autocheckpoint = 1000;
         PRAGMA mmap_size = 67108864;",
    )
    .map_err(|err| storage_err(&err))
}

pub(super) const MAX_READ_LIMIT: usize = 200;

pub(super) fn clamp_read_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_READ_LIMIT)
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        AppError, AppSettings, ContentKind, EntryFactory, EntryId, EntryRepository, RankReason,
        RecentOrder, RepresentationDataRef, RepresentationRole, SearchFilters, SearchMode,
        SearchQuery, Sensitivity, SettingsRepository, ThumbnailRecord,
    };
    use nagori_search::{MAX_NGRAM_INPUT_CHARS, normalize_text};
    use rusqlite::params;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    use super::schema::SCHEMA_VERSION;
    use super::*;

    async fn insert_text(store: &SqliteStore, text: &str) -> EntryId {
        let mut entry = EntryFactory::from_text(text);
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        store.insert(entry).await.unwrap()
    }

    #[test]
    fn fts_query_wraps_alnum_tokens_in_quotes() {
        assert_eq!(fts_query("hello world"), r#""hello" "world""#);
    }

    #[test]
    fn fts_query_strips_fts5_metacharacters() {
        // `(`, `)`, `:`, `*`, `"` are all FTS5-meaningful outside a
        // phrase string. They must not survive into the rendered MATCH
        // expression — even quoted, an unmatched `"` would corrupt the
        // expression, and `:` could be parsed as a column filter when
        // we later switch to column-scoped queries.
        assert_eq!(fts_query("foo:bar"), r#""foo" "bar""#);
        assert_eq!(fts_query("foo*"), r#""foo""#);
        assert_eq!(fts_query("(foo)"), r#""foo""#);
        assert_eq!(fts_query(r#"say "hi""#), r#""say" "hi""#);
    }

    #[test]
    fn fts_query_returns_empty_for_pure_punctuation() {
        // A query that collapses to zero tokens must produce the empty
        // string so the caller can short-circuit before issuing an
        // invalid FTS5 MATCH (the tokenizer would otherwise reject a
        // phrase that yields no terms).
        assert!(fts_query("(").is_empty());
        assert!(fts_query(":*").is_empty());
        assert!(fts_query("\"\"").is_empty());
        assert!(fts_query("   ").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_does_not_chmod_existing_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        // 0o750: group-readable but not group/other-writable, so it passes
        // the privacy validation and must be left untouched (not chmodded).
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).unwrap();

        ensure_private_directory(&shared).unwrap();

        let mode = std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_rejects_world_writable_without_sticky() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        // World-writable without the sticky bit lets a co-tenant plant a
        // socket/symlink at our endpoint — must be rejected.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();

        let err = ensure_private_directory(&shared).unwrap_err();

        assert!(
            err.to_string().contains("group/other-writable"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_allows_world_writable_with_sticky() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        // Sticky + world-writable mirrors `/tmp`: deletion/rename is restricted
        // to the owner, so a custom endpoint under it stays usable.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();

        ensure_private_directory(&shared).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_creates_missing_leaf_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let leaf = temp.path().join("nagori").join("ipc");

        ensure_private_directory(&leaf).unwrap();

        let mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn pre_create_db_file_private_rejects_symlinked_path() {
        let temp = tempfile::tempdir().unwrap();
        let bystander = temp.path().join("victim");
        std::fs::write(&bystander, b"do-not-touch").unwrap();
        let db_path = temp.path().join("nagori.db");
        std::os::unix::fs::symlink(&bystander, &db_path).unwrap();

        let err = pre_create_db_file_private(&db_path).unwrap_err();

        assert!(
            err.to_string().contains("is a symlink"),
            "unexpected error: {err}"
        );
        // The symlink target must be untouched (not chmodded or truncated).
        assert_eq!(std::fs::read(&bystander).unwrap(), b"do-not-touch");
    }

    #[cfg(unix)]
    #[test]
    fn pre_create_db_file_private_rejects_shared_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        // Sticky + world-writable (like `/tmp`): tolerated for IPC, but the DB
        // must refuse it because a co-tenant could race a sidecar symlink.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let db_path = shared.join("nagori.db");

        let err = pre_create_db_file_private(&db_path).unwrap_err();

        assert!(
            err.to_string().contains("group/other-writable"),
            "unexpected error: {err}"
        );
        // Nothing was created in the rejected directory.
        assert!(!db_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn pre_create_db_file_private_rejects_symlinked_wal_sidecar() {
        // A co-tenant can't necessarily plant the main DB path, but the WAL
        // sidecar SQLite creates later is just as dangerous: rejecting it must
        // happen before `journal_mode = WAL` opens it.
        let temp = tempfile::tempdir().unwrap();
        let bystander = temp.path().join("victim");
        std::fs::write(&bystander, b"do-not-touch").unwrap();
        let db_path = temp.path().join("nagori.db");
        let wal_path = temp.path().join("nagori.db-wal");
        std::os::unix::fs::symlink(&bystander, &wal_path).unwrap();

        let err = pre_create_db_file_private(&db_path).unwrap_err();

        assert!(
            err.to_string().contains("is a symlink"),
            "unexpected error: {err}"
        );
        assert_eq!(std::fs::read(&bystander).unwrap(), b"do-not-touch");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_rejects_symlinked_directory() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = ensure_private_directory(&link).unwrap_err();

        assert!(err.to_string().contains("is a symlink"));
    }

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

    #[tokio::test]
    async fn settings_revision_bumps_and_cas_rejects_stale_writes() {
        let store = SqliteStore::open_memory().unwrap();
        // Fresh store: no settings row yet, so the revision baseline is 0 and
        // the body is the default — read as one consistent pair.
        let (settings, revision) = store.get_settings_with_revision().await.unwrap();
        assert_eq!(revision, 0);
        assert_eq!(settings, AppSettings::default());

        // A compare-and-swap save against the current (0) revision lands and
        // advances the token to 1.
        let rev = store
            .save_settings_checked(AppSettings::default(), 0)
            .await
            .unwrap();
        assert_eq!(rev, 1);
        assert_eq!(store.get_settings_with_revision().await.unwrap().1, 1);

        // A second save still using the stale base (0) is a conflict — this is
        // the lost-update a full-blob client would otherwise cause — and the
        // stored revision is left untouched.
        let err = store
            .save_settings_checked(AppSettings::default(), 0)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        assert_eq!(store.get_settings_with_revision().await.unwrap().1, 1);

        // Re-reading the current revision and retrying succeeds.
        let rev = store
            .save_settings_checked(AppSettings::default(), 1)
            .await
            .unwrap();
        assert_eq!(rev, 2);

        // A plain (force) save — the path the tray toggle / IPC client take —
        // also advances the revision, so a stale full-blob base is caught.
        store.save_settings(AppSettings::default()).await.unwrap();
        assert_eq!(store.get_settings_with_revision().await.unwrap().1, 3);
    }

    #[tokio::test]
    async fn stores_and_searches_japanese_text() {
        let store = SqliteStore::open_memory().unwrap();
        let mut entry = EntryFactory::from_text("クリップボード履歴");
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        let id = store.insert(entry).await.unwrap();

        let query = SearchQuery::new("クリップ", normalize_text("クリップ"), 10);
        let results = store.search(query).await.unwrap();
        assert_eq!(results[0].entry_id, id);
    }

    #[tokio::test]
    async fn katakana_entry_is_found_by_hiragana_query() {
        // Kana folding lives in the ngram generator: a Katakana clip and a
        // Hiragana query share folded grams even though `normalize_text` (NFKC)
        // leaves the two scripts distinct.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "クリップボード履歴").await;

        let query = SearchQuery::new("くりっぷ", normalize_text("くりっぷ"), 10);
        let results = store.search(query).await.unwrap();
        assert!(
            results.iter().any(|r| r.entry_id == id),
            "hiragana query should recall the katakana entry via folded ngrams",
        );
    }

    #[tokio::test]
    async fn single_kanji_query_recalls_entry() {
        // A lone ideograph matches the document-side Han 1-gram, so it recalls
        // even though `unicode61` FTS collapses the run to one token and the
        // 2/3-gram path needs ≥ 2 chars.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "設計資料のメモ").await;

        let query = SearchQuery::new("設", normalize_text("設"), 10);
        let results = store.search(query).await.unwrap();
        assert!(
            results.iter().any(|r| r.entry_id == id),
            "single-kanji query should recall the entry via the Han 1-gram",
        );
    }

    #[tokio::test]
    async fn rebuild_stale_ngrams_drains_and_restamps() {
        // Simulate a pre-upgrade document: grams produced by an older generator
        // and a stale per-row version marker. The background rebuild must
        // regenerate the grams from the stored normalized_text and restamp the
        // row, without touching normalized_text / preview.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "設計資料のメモ").await;
        let (preview_before, normalized_before) = {
            let conn = store.conn().unwrap();
            conn.execute("DELETE FROM ngrams", []).unwrap();
            conn.execute("UPDATE search_documents SET ngram_index_version = 0", [])
                .unwrap();
            conn.query_row(
                "SELECT preview, normalized_text FROM search_documents WHERE entry_id = ?1",
                rusqlite::params![id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap()
        };

        assert_eq!(store.pending_ngram_rebuild().await.unwrap(), 1);

        let mut drained = 0;
        loop {
            let n = store.rebuild_stale_ngrams().await.unwrap();
            if n == 0 {
                break;
            }
            drained += n;
        }
        assert_eq!(drained, 1);
        assert_eq!(store.pending_ngram_rebuild().await.unwrap(), 0);

        // Grams regenerated, and preview/normalized_text untouched.
        let conn = store.conn().unwrap();
        let gram_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ngrams", [], |row| row.get(0))
            .unwrap();
        assert!(gram_count > 0, "rebuild should regenerate grams");
        let (preview_after, normalized_after): (String, String) = conn
            .query_row(
                "SELECT preview, normalized_text FROM search_documents WHERE entry_id = ?1",
                rusqlite::params![id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(preview_before, preview_after);
        assert_eq!(normalized_before, normalized_after);
    }

    #[tokio::test]
    async fn duplicate_insert_returns_existing_id() {
        let store = SqliteStore::open_memory().unwrap();
        let first_id = insert_text(&store, "same clipboard value").await;
        let second_id = insert_text(&store, "same clipboard value").await;

        assert_eq!(second_id, first_id);
        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);

        let query = SearchQuery::new("clipboard", normalize_text("clipboard"), 10);
        let results = store.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, first_id);
    }

    #[tokio::test]
    async fn duplicate_insert_with_identical_reps_refreshes_source() {
        // Dedupe is keyed on `representation_set_hash`, so two snapshots
        // with the same primary AND the same alternatives collide. The
        // dedupe path must then refresh the entries row's source/sensitivity
        // columns and bump `created_at`/`updated_at` so the source_app
        // filter sees the latest copy — not the first one.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, SourceApp,
        };

        let store = SqliteStore::open_memory().unwrap();

        let make_snapshot = |bundle: &str| ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("dedupe-rewrite"),
            captured_at: OffsetDateTime::now_utc(),
            source: Some(SourceApp {
                bundle_id: Some(bundle.to_owned()),
                name: Some(bundle.to_owned()),
                executable_path: None,
            }),
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>shared</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("shared body".to_owned()),
                },
            ],
        };

        let first = EntryFactory::from_snapshot(make_snapshot("com.example.editor"))
            .expect("first snapshot");
        let first_id = store.insert(first).await.unwrap();

        let second = EntryFactory::from_snapshot(make_snapshot("com.example.terminal"))
            .expect("second snapshot");
        let second_id = store.insert(second).await.unwrap();
        assert_eq!(second_id, first_id, "dedupe should reuse the row");

        let fetched = store.get(first_id).await.unwrap().expect("row exists");
        let source = fetched.metadata.source.as_ref().expect("source preserved");
        assert_eq!(source.bundle_id.as_deref(), Some("com.example.terminal"));

        let mut query = SearchQuery::new("shared", normalize_text("shared"), 10);
        query.filters = SearchFilters {
            source_app: Some("com.example.terminal".to_owned()),
            ..Default::default()
        };
        let hits = store.search(query).await.unwrap();
        assert_eq!(hits.len(), 1, "source filter must hit the new source");
        assert_eq!(hits[0].entry_id, first_id);
    }

    #[tokio::test]
    async fn distinct_alternatives_produce_distinct_rows() {
        // Two snapshots with the same primary text but different HTML
        // alternatives must land in distinct rows, otherwise the later
        // capture would silently overwrite the earlier row's
        // alternatives.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, SourceApp,
        };

        let store = SqliteStore::open_memory().unwrap();

        let make_snapshot = |html: &str| ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("distinct-alts"),
            captured_at: OffsetDateTime::now_utc(),
            source: Some(SourceApp {
                bundle_id: Some("com.example.editor".to_owned()),
                name: Some("editor".to_owned()),
                executable_path: None,
            }),
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text(html.to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("shared body".to_owned()),
                },
            ],
        };

        let first =
            EntryFactory::from_snapshot(make_snapshot("<p>v1</p>")).expect("first snapshot");
        let first_id = store.insert(first).await.unwrap();
        let second =
            EntryFactory::from_snapshot(make_snapshot("<p>v2</p>")).expect("second snapshot");
        let second_id = store.insert(second).await.unwrap();

        assert_ne!(
            first_id, second_id,
            "different alternative sets must not collapse onto one row"
        );
        let first_reps = store.list_representations(first_id).await.unwrap();
        let first_html = first_reps
            .iter()
            .find(|r| r.mime_type == "text/html")
            .expect("first html rep present");
        match &first_html.data {
            nagori_core::RepresentationDataRef::InlineText(text) => {
                assert_eq!(text, "<p>v1</p>", "first row keeps its original html");
            }
            other => panic!("expected inline text rep, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pin_round_trip() {
        let store = SqliteStore::open_memory().unwrap();
        let id = store
            .insert(EntryFactory::from_text("hello"))
            .await
            .unwrap();
        store.set_pinned(id, true).await.unwrap();
        let pinned = store.list_pinned().await.unwrap();
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].lifecycle.pinned);
    }

    #[tokio::test]
    async fn list_pinned_excludes_blocked_rows() {
        // The capture path refuses to persist `Blocked`, but stale rows
        // from older daemons or hand-edited DBs can survive. Match
        // `list_recent` / `search` and keep them out of default lists so
        // the DTO layer never has to ship a raw-text preview from one.
        let store = SqliteStore::open_memory().unwrap();
        let pinned_public = insert_text(&store, "public pinned").await;
        store.set_pinned(pinned_public, true).await.unwrap();
        let mut blocked = EntryFactory::from_text("blocked pinned");
        blocked.search.normalized_text = normalize_text(blocked.plain_text().unwrap());
        blocked.sensitivity = Sensitivity::Blocked;
        let blocked_id = store.insert(blocked).await.unwrap();
        store.set_pinned(blocked_id, true).await.unwrap();

        let pinned = store.list_pinned().await.unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].id, pinned_public);
    }

    #[tokio::test]
    async fn pinned_only_filter_excludes_others() {
        let store = SqliteStore::open_memory().unwrap();
        let pinned_id = insert_text(&store, "pinned snippet").await;
        store.set_pinned(pinned_id, true).await.unwrap();
        let _other = insert_text(&store, "regular snippet").await;

        let mut query = SearchQuery::new("snippet", normalize_text("snippet"), 10);
        query.filters = SearchFilters {
            pinned_only: true,
            ..Default::default()
        };
        let results = store.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, pinned_id);
    }

    #[tokio::test]
    async fn exact_mode_skips_fts_only_matches() {
        let store = SqliteStore::open_memory().unwrap();
        let _ = insert_text(&store, "the quick brown fox").await;

        let mut query = SearchQuery::new("qui ck", normalize_text("qui ck"), 10);
        query.mode = SearchMode::Exact;
        let exact = store.search(query.clone()).await.unwrap();
        assert!(exact.is_empty());

        // Fuzzy keeps ASCII ngram recall, so the whitespace-insensitive match
        // on `quick` still surfaces there. Auto deliberately does not — see
        // `auto_skips_ascii_ngram_only_match`.
        query.mode = SearchMode::Fuzzy;
        let fuzzy = store.search(query).await.unwrap();
        assert!(!fuzzy.is_empty());
    }

    #[tokio::test]
    async fn auto_skips_ascii_ngram_only_match() {
        // Regression for the ngram fan-out fix: the Auto/Hybrid plan must
        // not run ASCII ngram. `qui ck` reaches `the quick brown fox` only via
        // whitespace-stripped ngram overlap (`quick`) — FTS sees the tokens
        // `qui`/`ck` with no whole-token match, and the substring scan looks
        // for the literal `qui ck`. So Auto now returns nothing; ASCII
        // partial/typo recall lives in explicit Fuzzy.
        let store = SqliteStore::open_memory().unwrap();
        let _ = insert_text(&store, "the quick brown fox").await;

        let mut query = SearchQuery::new("qui ck", normalize_text("qui ck"), 10);
        query.mode = SearchMode::Auto;
        let auto = store.search(query).await.unwrap();
        assert!(
            auto.is_empty(),
            "Auto no longer chases ASCII ngram-only matches",
        );
    }

    #[tokio::test]
    async fn exact_substring_walks_full_corpus_unbounded() {
        // Regression: an earlier iteration capped the substring CTE to the
        // most recent SUBSTRING_SCAN_WINDOW rows for *all* plans, which
        // silently dropped exact matches outside the window. The Exact
        // plan must always see the full live corpus because nothing else
        // (FTS / ngram) backstops it.
        use nagori_core::SearchCandidateProvider;
        use tokio_util::sync::CancellationToken;
        let store = SqliteStore::open_memory().unwrap();
        let _old = insert_text(&store, "needle in a haystack").await;
        for idx in 0..20 {
            let _ = insert_text(&store, &format!("filler {idx}")).await;
        }
        let cancel = CancellationToken::new();
        let bounded = store
            .substring_candidates("needle", &SearchFilters::default(), 10, true, &cancel)
            .await
            .unwrap();
        let unbounded = store
            .substring_candidates("needle", &SearchFilters::default(), 10, false, &cancel)
            .await
            .unwrap();
        // Both still find it on a 21-row DB (window is 5000), but the
        // unbounded path is what's used for explicit `Exact` searches —
        // confirming both shapes return the row guards against future
        // regressions where the bounded path swallows older matches.
        assert_eq!(bounded.len(), 1);
        assert_eq!(unbounded.len(), 1);
    }

    #[tokio::test]
    async fn ngram_cjk_only_mode_drops_ascii_grams() {
        // Directly pins the gram filter. `CjkOnly` (the Auto/Hybrid policy)
        // keeps only CJK-bearing grams, so a pure-ASCII query yields nothing
        // while a mixed query still matches on its CJK / boundary grams. The
        // `Full` mode (explicit Fuzzy) keeps ASCII grams so the same ASCII
        // query matches there.
        use nagori_core::{NgramQueryMode, SearchCandidateProvider};
        use tokio_util::sync::CancellationToken;
        let store = SqliteStore::open_memory().unwrap();
        let cancel = CancellationToken::new();
        let mixed = {
            let mut entry = EntryFactory::from_text("メモ alpha 設計");
            entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
            store.insert(entry).await.unwrap()
        };
        let ascii = insert_text(&store, "needle in a haystack").await;

        // Pure-ASCII query under CjkOnly: every gram is filtered out → empty.
        let ascii_cjk_only = store
            .ngram_candidates(
                "needle",
                &SearchFilters::default(),
                10,
                NgramQueryMode::CjkOnly,
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            ascii_cjk_only.is_empty(),
            "CjkOnly must drop every ASCII gram",
        );

        // Same ASCII query under Full still matches via the full gram set.
        let ascii_full = store
            .ngram_candidates(
                "needle",
                &SearchFilters::default(),
                10,
                NgramQueryMode::Full,
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            ascii_full.iter().any(|c| c.entry.id == ascii),
            "Full keeps ASCII grams so the ASCII entry still matches",
        );

        // Mixed query under CjkOnly keeps the `設計` / boundary grams, so the
        // mixed entry is still reachable through ngram alone.
        let mixed_cjk_only = store
            .ngram_candidates(
                &normalize_text("alpha 設計"),
                &SearchFilters::default(),
                10,
                NgramQueryMode::CjkOnly,
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            mixed_cjk_only.iter().any(|c| c.entry.id == mixed),
            "CjkOnly must keep CJK-bearing grams for mixed queries",
        );
    }

    #[tokio::test]
    async fn run_search_blocking_interrupts_a_cancelled_query() {
        use std::time::Duration;

        use tokio_util::sync::CancellationToken;

        let store = SqliteStore::open_memory().unwrap();
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        // Cancel shortly after the heavy query starts so the abort lands
        // mid-flight rather than before the connection is even acquired.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });

        // A recursive CTE that would run effectively forever; the cancellation
        // progress-handler installed by `run_search_blocking` must abort it so
        // the call returns promptly with an error instead of running to
        // completion and pinning the pooled connection.
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            store.run_search_blocking(&cancel, |conn| {
                let count: i64 = conn
                    .query_row(
                        "WITH RECURSIVE c(x) AS (
                             SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 100000000000
                         )
                         SELECT count(*) FROM c",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|err| AppError::Storage(err.to_string()))?;
                Ok(count)
            }),
        )
        .await
        .expect("a cancelled query must be interrupted rather than run to completion");
        assert!(
            result.is_err(),
            "interrupting the query must surface as an error, got {result:?}",
        );

        // The connection is back in the pool, so a fresh query still works.
        let ok = store
            .run_search_blocking(&CancellationToken::new(), |conn| {
                conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                    .map_err(|err| AppError::Storage(err.to_string()))
            })
            .await
            .expect("the connection should be reusable after an interrupt");
        assert_eq!(ok, 1);
    }

    #[tokio::test]
    async fn kind_filter_limits_to_url_entries() {
        let store = SqliteStore::open_memory().unwrap();
        let _ = insert_text(&store, "https://example.com/foo").await;
        let _ = insert_text(&store, "plain text foo").await;

        let mut query = SearchQuery::new("foo", normalize_text("foo"), 10);
        query.filters = SearchFilters {
            kinds: vec![ContentKind::Url],
            ..Default::default()
        };
        let results = store.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_kind, ContentKind::Url);
    }

    async fn audit_kind_count(store: &SqliteStore, kind: &str) -> i64 {
        store.audit_event_count(kind).await.expect("audit count")
    }

    #[tokio::test]
    async fn insert_records_ngram_truncated_when_input_exceeds_cap() {
        // The ngram index silently caps at `MAX_NGRAM_INPUT_CHARS` so a paste
        // larger than the cap loses fuzzy-search recall on its tail. The
        // user-visible symptom — "search misses the bottom of my pasted
        // doc" — was previously invisible at the DB layer; this audit event
        // is the only artefact that survives log rotation and lets a future
        // support investigation correlate "missing matches" with the
        // specific entry that was truncated.
        let store = SqliteStore::open_memory().unwrap();
        let oversized: String = "a".repeat(MAX_NGRAM_INPUT_CHARS + 1);
        let _ = insert_text(&store, &oversized).await;

        assert_eq!(audit_kind_count(&store, "ngram_truncated").await, 1);
    }

    #[tokio::test]
    async fn insert_skips_audit_when_input_fits_cap() {
        // Negative case: an entry that fits inside the cap must not emit an
        // audit row, otherwise the events table fills up with noise on
        // every paste and obscures the genuine truncation signal.
        let store = SqliteStore::open_memory().unwrap();
        let _ = insert_text(&store, "a short paste").await;

        assert_eq!(audit_kind_count(&store, "ngram_truncated").await, 0);
    }

    #[tokio::test]
    async fn retention_delete_prunes_search_tables() {
        let store = SqliteStore::open_memory().unwrap();
        let _ = insert_text(&store, "temporary searchable value").await;

        let deleted = store
            .clear_older_than(OffsetDateTime::now_utc() + time::Duration::seconds(1))
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let conn = store.conn().unwrap();
        for table in ["search_documents", "search_fts", "ngrams"] {
            let sql = format!("SELECT COUNT(*) FROM {table}");
            let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
            assert_eq!(count, 0, "{table} should be pruned");
        }
    }

    /// Backdate the `created_at` timestamp on a row so that retention
    /// windows (`clear_older_than`) and `enforce_retention_count` ordering
    /// can be tested deterministically without sleeping.
    fn backdate_entry(store: &SqliteStore, id: EntryId, when: OffsetDateTime) {
        let formatted = when.format(&Rfc3339).expect("rfc3339 format");
        let conn = store.conn().expect("lock conn");
        conn.execute(
            "UPDATE entries SET created_at = ?1 WHERE id = ?2",
            params![formatted, id.to_string()],
        )
        .expect("backdate row");
    }

    fn count_active(store: &SqliteStore) -> i64 {
        let conn = store.conn().expect("lock conn");
        conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("count active")
    }

    fn count_total(store: &SqliteStore) -> i64 {
        let conn = store.conn().expect("lock conn");
        conn.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .expect("count total")
    }

    #[tokio::test]
    async fn enforce_retention_count_drops_oldest_unpinned() {
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let oldest = insert_text(&store, "oldest entry").await;
        let middle = insert_text(&store, "middle entry").await;
        let newest = insert_text(&store, "newest entry").await;
        backdate_entry(&store, oldest, now - time::Duration::days(3));
        backdate_entry(&store, middle, now - time::Duration::days(2));
        backdate_entry(&store, newest, now - time::Duration::days(1));

        let removed = store.enforce_retention_count(2).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(count_active(&store), 2);

        let surviving = store
            .list_recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert!(surviving.contains(&middle));
        assert!(surviving.contains(&newest));
        assert!(!surviving.contains(&oldest));

        // Idempotent: a second call with the same cap removes nothing.
        assert_eq!(store.enforce_retention_count(2).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn enforce_retention_count_hard_deletes_evicted_rows() {
        // Retention eviction must *physically* remove the row — and its
        // representations / search index via `ON DELETE CASCADE` — rather than
        // tombstone it. A soft delete left the body, blobs, and embeddings on
        // disk, so a retention cap never reclaimed space and the content stayed
        // recoverable from the file.
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let oldest = insert_text(&store, "oldest entry").await;
        let _middle = insert_text(&store, "middle entry").await;
        let _newest = insert_text(&store, "newest entry").await;
        backdate_entry(&store, oldest, now - time::Duration::days(3));

        assert_eq!(count_total(&store), 3);
        let removed = store.enforce_retention_count(2).await.unwrap();
        assert_eq!(removed, 1);

        // The evicted row is gone from the table entirely, not just filtered
        // out by `deleted_at`.
        assert_eq!(count_total(&store), 2);
        assert_eq!(count_active(&store), 2);
        let conn = store.conn().expect("lock conn");
        let surviving: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE id = ?1",
                params![oldest.to_string()],
                |row| row.get(0),
            )
            .expect("count evicted row");
        assert_eq!(surviving, 0, "evicted row must be physically deleted");
        // Its representation rows cascade away with it.
        let reps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
                params![oldest.to_string()],
                |row| row.get(0),
            )
            .expect("count evicted representations");
        assert_eq!(
            reps, 0,
            "evicted row's representations must cascade-delete with it",
        );
    }

    #[tokio::test]
    async fn enforce_retention_count_keeps_pinned_above_cap() {
        // Pinned entries never count toward the eviction window: the
        // OFFSET-based delete only sees unpinned rows, so a single pinned
        // ancient row plus N unpinned rows yields exactly N retained.
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let pinned_old = insert_text(&store, "pinned ancient").await;
        let oldest = insert_text(&store, "regular oldest").await;
        let middle = insert_text(&store, "regular middle").await;
        let newest = insert_text(&store, "regular newest").await;
        backdate_entry(&store, pinned_old, now - time::Duration::days(10));
        backdate_entry(&store, oldest, now - time::Duration::days(3));
        backdate_entry(&store, middle, now - time::Duration::days(2));
        backdate_entry(&store, newest, now - time::Duration::days(1));
        store.set_pinned(pinned_old, true).await.unwrap();

        let removed = store.enforce_retention_count(1).await.unwrap();
        assert_eq!(removed, 2);

        let active_ids = store
            .list_recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert!(active_ids.contains(&pinned_old), "pinned must survive");
        assert!(active_ids.contains(&newest), "newest unpinned must survive");
        assert!(!active_ids.contains(&middle));
        assert!(!active_ids.contains(&oldest));
    }

    #[tokio::test]
    async fn clear_older_than_skips_pinned() {
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let pinned = insert_text(&store, "pinned ancient").await;
        let stale = insert_text(&store, "stale ancient").await;
        let fresh = insert_text(&store, "fresh value").await;
        backdate_entry(&store, pinned, now - time::Duration::days(40));
        backdate_entry(&store, stale, now - time::Duration::days(40));
        backdate_entry(&store, fresh, now - time::Duration::days(1));
        store.set_pinned(pinned, true).await.unwrap();

        let removed = store
            .clear_older_than(now - time::Duration::days(7))
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let surviving = store
            .list_recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert!(surviving.contains(&pinned), "pinned should survive cutoff");
        assert!(surviving.contains(&fresh), "fresh row must remain");
        assert!(!surviving.contains(&stale), "stale row should be cleared");
    }

    #[tokio::test]
    async fn clear_non_pinned_purges_only_unpinned_rows() {
        let store = SqliteStore::open_memory().unwrap();
        let pinned = insert_text(&store, "pinned anchor").await;
        let unpinned_a = insert_text(&store, "ephemeral one").await;
        let unpinned_b = insert_text(&store, "ephemeral two").await;
        store.set_pinned(pinned, true).await.unwrap();

        let removed = store.clear_non_pinned().await.unwrap();
        assert_eq!(removed, 2);

        let surviving = store
            .list_recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert_eq!(surviving, vec![pinned], "only pinned row must survive");
        assert!(!surviving.contains(&unpinned_a));
        assert!(!surviving.contains(&unpinned_b));
    }

    #[tokio::test]
    async fn clear_non_pinned_hard_deletes_unpinned_content() {
        // "Clear history" / clear-on-quit must physically purge non-pinned
        // rows (body, representations, search index) so nothing is
        // recoverable from the live table — while the pinned row keeps its
        // content intact.
        let store = SqliteStore::open_memory().unwrap();
        let pinned = insert_text(&store, "pinned anchor").await;
        let unpinned = insert_text(&store, "ephemeral secret").await;
        store.set_pinned(pinned, true).await.unwrap();

        let removed = store.clear_non_pinned().await.unwrap();
        assert_eq!(removed, 1);

        // Only the pinned row remains anywhere in the table.
        assert_eq!(count_total(&store), 1);
        let conn = store.conn().unwrap();
        for (table, column) in [
            ("entries", "id"),
            ("entry_representations", "entry_id"),
            ("search_documents", "entry_id"),
            ("ngrams", "entry_id"),
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
            let count: i64 = conn
                .query_row(&sql, params![unpinned.to_string()], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} rows for the cleared entry must be gone");
        }
        // The pinned row's content survives.
        let pinned_reps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
                params![pinned.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(pinned_reps > 0, "pinned row must keep its representations");
    }

    #[tokio::test]
    async fn purge_deleted_hard_deletes_tombstones_including_pinned() {
        // `mark_deleted` only tombstones; `purge_deleted` is the deferred
        // reclaim. It must physically drop *every* tombstoned row — including a
        // pinned one, which no `pinned = 0` retention path would ever reach —
        // while leaving a live (non-deleted) row untouched. Without it a
        // "delete this pinned secret" would keep its body/blobs on disk forever.
        let store = SqliteStore::open_memory().unwrap();
        let pinned_deleted = insert_text(&store, "pinned secret to delete").await;
        let plain_deleted = insert_text(&store, "ordinary deleted").await;
        let live = insert_text(&store, "still here").await;
        store.set_pinned(pinned_deleted, true).await.unwrap();
        store.mark_deleted(pinned_deleted).await.unwrap();
        store.mark_deleted(plain_deleted).await.unwrap();

        // Soft delete leaves the rows on disk, just hidden from live queries.
        assert_eq!(count_total(&store), 3);
        assert_eq!(count_active(&store), 1);

        let purged = store.purge_deleted().await.unwrap();
        assert_eq!(
            purged, 2,
            "both tombstones (incl. the pinned one) must be reclaimed",
        );

        // Only the live row remains anywhere in the table, with its content.
        assert_eq!(count_total(&store), 1);
        assert_eq!(count_active(&store), 1);
        let conn = store.conn().unwrap();
        for id in [pinned_deleted, plain_deleted] {
            for (table, column) in [
                ("entries", "id"),
                ("entry_representations", "entry_id"),
                ("search_documents", "entry_id"),
                ("ngrams", "entry_id"),
            ] {
                let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
                let count: i64 = conn
                    .query_row(&sql, params![id.to_string()], |row| row.get(0))
                    .unwrap();
                assert_eq!(count, 0, "{table} rows for the purged entry must be gone");
            }
        }
        let live_reps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
                params![live.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(live_reps > 0, "the live row must keep its representations");
        drop(conn);

        // Idempotent: a second purge with no tombstones removes nothing.
        assert_eq!(store.purge_deleted().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn reinserting_after_mark_deleted_creates_new_row() {
        // The content-hash UNIQUE index is `WHERE deleted_at IS NULL`, so
        // tombstoned rows must not block re-inserts of the same text.
        let store = SqliteStore::open_memory().unwrap();
        let original = insert_text(&store, "duplicated value").await;
        store.mark_deleted(original).await.unwrap();
        assert_eq!(count_active(&store), 0);

        let revived = insert_text(&store, "duplicated value").await;
        assert_ne!(
            revived, original,
            "soft-deleted hash must not be reused as the live id",
        );

        // Tombstone is preserved alongside the new active row.
        assert_eq!(count_active(&store), 1);
        assert_eq!(count_total(&store), 2);

        // The fresh row owns the search artefacts and is queryable.
        let query = SearchQuery::new("duplicated", normalize_text("duplicated"), 10);
        let results = store.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, revived);
    }

    async fn insert_with_source(store: &SqliteStore, text: &str, bundle: &str) -> EntryId {
        let mut entry = EntryFactory::from_text(text);
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        entry.metadata.source = Some(nagori_core::SourceApp {
            bundle_id: Some(bundle.to_owned()),
            name: None,
            executable_path: None,
        });
        store.insert(entry).await.unwrap()
    }

    #[tokio::test]
    async fn recent_mode_returns_pinned_first_then_chronological() {
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let oldest = insert_text(&store, "alpha row").await;
        let middle = insert_text(&store, "bravo row").await;
        let newest = insert_text(&store, "charlie row").await;
        backdate_entry(&store, oldest, now - time::Duration::hours(3));
        backdate_entry(&store, middle, now - time::Duration::hours(2));
        backdate_entry(&store, newest, now - time::Duration::hours(1));
        store.set_pinned(oldest, true).await.unwrap();

        let mut query = SearchQuery::new("", String::new(), 10);
        query.mode = SearchMode::Recent;
        query.recent_order = RecentOrder::PinnedFirstThenRecency;
        let results = store.search(query).await.unwrap();
        let ids = results.iter().map(|r| r.entry_id).collect::<Vec<_>>();
        assert_eq!(ids[0], oldest, "pinned row should rank first");
        assert!(ids.contains(&middle));
        assert!(ids.contains(&newest));
    }

    #[tokio::test]
    async fn recent_mode_can_order_by_use_count() {
        let store = SqliteStore::open_memory().unwrap();
        let low = insert_text(&store, "low use").await;
        let high = insert_text(&store, "high use").await;
        store.increment_use_count(high).await.unwrap();
        store.increment_use_count(high).await.unwrap();
        store.increment_use_count(low).await.unwrap();

        let mut query = SearchQuery::new("", String::new(), 10);
        query.mode = SearchMode::Recent;
        query.recent_order = RecentOrder::ByUseCount;
        let results = store.search(query).await.unwrap();

        assert_eq!(results.first().map(|r| r.entry_id), Some(high));
        assert!(
            results
                .first()
                .is_some_and(|r| r.rank_reason.contains(&RankReason::FrequentlyUsed)),
        );
    }

    #[tokio::test]
    async fn full_text_mode_matches_separated_tokens_in_any_order() {
        let store = SqliteStore::open_memory().unwrap();
        let target = insert_text(&store, "search relevance ranking notes").await;
        let _ = insert_text(&store, "completely unrelated note about lunch").await;

        let mut query = SearchQuery::new("ranking search", normalize_text("ranking search"), 10);
        query.mode = SearchMode::FullText;
        let results = store.search(query).await.unwrap();
        let hits = results.iter().map(|r| r.entry_id).collect::<Vec<_>>();
        assert!(
            hits.contains(&target),
            "FTS should find both terms regardless of order"
        );
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn fuzzy_mode_finds_partial_cjk_substring() {
        let store = SqliteStore::open_memory().unwrap();
        let target = {
            let mut entry = EntryFactory::from_text("クリップボード履歴の保存先");
            entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
            store.insert(entry).await.unwrap()
        };
        let _ = insert_text(&store, "完全に別の日本語の文章").await;

        let mut query = SearchQuery::new("ボード", normalize_text("ボード"), 10);
        query.mode = SearchMode::Fuzzy;
        let results = store.search(query).await.unwrap();
        assert!(results.iter().map(|r| r.entry_id).any(|x| x == target));
    }

    #[tokio::test]
    async fn mixed_cjk_ascii_query_finds_entries_in_auto_mode() {
        let store = SqliteStore::open_memory().unwrap();
        let target = {
            let mut entry = EntryFactory::from_text("メモ alpha 設計");
            entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
            store.insert(entry).await.unwrap()
        };
        let _ = insert_text(&store, "純粋な日本語のメモ").await;
        let _ = insert_text(&store, "english only note").await;

        let query = SearchQuery::new("alpha 設計", normalize_text("alpha 設計"), 10);
        // Auto plan exercises LIKE + FTS + fuzzy together.
        let results = store.search(query).await.unwrap();
        assert!(results.iter().map(|r| r.entry_id).any(|x| x == target));
    }

    #[tokio::test]
    async fn source_app_filter_isolates_by_bundle_id() {
        let store = SqliteStore::open_memory().unwrap();
        let editor =
            insert_with_source(&store, "shared keyword editor side", "com.example.editor").await;
        let _terminal = insert_with_source(
            &store,
            "shared keyword terminal side",
            "com.example.terminal",
        )
        .await;

        let mut query = SearchQuery::new("shared", normalize_text("shared"), 10);
        query.filters = SearchFilters {
            source_app: Some("com.example.editor".to_owned()),
            ..Default::default()
        };
        let results = store.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, editor);
    }

    #[tokio::test]
    async fn created_after_and_before_filters_clip_window() {
        let store = SqliteStore::open_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        let ancient = insert_text(&store, "window keyword ancient").await;
        let middle = insert_text(&store, "window keyword middle").await;
        let recent = insert_text(&store, "window keyword recent").await;
        backdate_entry(&store, ancient, now - time::Duration::days(10));
        backdate_entry(&store, middle, now - time::Duration::days(5));
        backdate_entry(&store, recent, now - time::Duration::days(1));

        let mut after_query = SearchQuery::new("window", normalize_text("window"), 10);
        after_query.filters = SearchFilters {
            created_after: Some(now - time::Duration::days(7)),
            ..Default::default()
        };
        let after_hits = store
            .search(after_query)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.entry_id)
            .collect::<Vec<_>>();
        assert!(after_hits.contains(&middle));
        assert!(after_hits.contains(&recent));
        assert!(!after_hits.contains(&ancient));

        let mut before_query = SearchQuery::new("window", normalize_text("window"), 10);
        before_query.filters = SearchFilters {
            created_before: Some(now - time::Duration::days(3)),
            ..Default::default()
        };
        let before_hits = store
            .search(before_query)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.entry_id)
            .collect::<Vec<_>>();
        assert!(before_hits.contains(&ancient));
        assert!(before_hits.contains(&middle));
        assert!(!before_hits.contains(&recent));
    }

    #[tokio::test]
    async fn image_payload_round_trip() {
        use nagori_core::{
            ClipboardContent, ClipboardData, ClipboardRepresentation, ClipboardSequence,
            ClipboardSnapshot,
        };

        let bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("img-1"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(bytes.clone()),
            }],
        };
        let entry =
            EntryFactory::from_snapshot(snapshot).expect("snapshot should yield image entry");
        let id = entry.id;
        let stored = SqliteStore::open_memory().unwrap();
        let returned_id = stored.insert(entry).await.unwrap();
        assert_eq!(returned_id, id);

        let payload = stored.get_payload(id).await.unwrap();
        assert_eq!(payload, Some((bytes, "image/png".to_owned())));

        // The deserialised entry must keep its mime type and byte count, and
        // `pending_bytes` must be `None` after the round-trip — the bytes now
        // live in `entry_representations.payload_blob`, not inside `content_json`.
        let fetched = stored.get(id).await.unwrap().expect("row exists");
        match &fetched.content {
            ClipboardContent::Image(img) => {
                assert_eq!(img.byte_count, 12);
                assert_eq!(img.mime_type.as_deref(), Some("image/png"));
                assert!(img.pending_bytes.is_none());
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_alternate_image_payload_returns_file_list_image() {
        // A presentation copied from Finder: the file URL is the primary
        // representation, an `image/png` render rides along as an
        // alternative. The thumbnail generator reaches the image through
        // this lookup because the primary-only `get_payload` can't.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let image = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4]; // PNG signature
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("file-list-with-image"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/uri-list".to_owned(),
                    data: ClipboardData::FilePaths(vec!["/Users/me/deck.pptx".to_owned()]),
                },
                ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(image.clone()),
                },
            ],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield file list");
        let id = entry.id;
        let store = SqliteStore::open_memory().unwrap();
        store.insert(entry).await.unwrap();

        // The primary is the file URL list (text), so the primary-only
        // lookup finds nothing...
        assert_eq!(store.get_payload(id).await.unwrap(), None);
        // ...but the accompanying image is reachable for the thumbnail path.
        assert_eq!(
            store.get_alternate_image_payload(id).await.unwrap(),
            Some((image, "image/png".to_owned())),
        );
    }

    #[tokio::test]
    async fn get_alternate_image_payload_ignores_non_image_alternatives() {
        // An HTML + plain clip carries alternatives, but none are images, so
        // the image-only lookup must return None rather than a text row.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("text-no-image"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hi".to_owned()),
                },
            ],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let id = entry.id;
        let store = SqliteStore::open_memory().unwrap();
        store.insert(entry).await.unwrap();

        assert_eq!(store.get_alternate_image_payload(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn snapshot_multi_rep_writes_one_row_per_representation() {
        // HTML + plain + RTF snapshot must produce three persisted rows so
        // a later copy-back path can re-publish whichever flavour the user
        // (or the receiving app) asks for. Without this, the multi-rep
        // promise collapses back to primary-only and pasting into a
        // markup-aware target loses the rich formatting the source offered.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("multi-rep-store"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hi".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "application/rtf".to_owned(),
                    data: ClipboardData::Text("{\\rtf1 hi}".to_owned()),
                },
            ],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let store = SqliteStore::open_memory().unwrap();
        let id = store.insert(entry).await.unwrap();

        let conn = store.conn().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT role, mime_type, ordinal, text_content
                 FROM entry_representations
                 WHERE entry_id = ?1
                 ORDER BY ordinal ASC",
            )
            .unwrap();
        let rows: Vec<(String, String, i64, Option<String>)> = stmt
            .query_map([id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "primary");
        assert_eq!(rows[0].1, "text/html");
        assert_eq!(rows[0].2, 0);
        assert_eq!(rows[0].3.as_deref(), Some("<p>hi</p>"));

        assert_eq!(rows[1].0, "plain_fallback");
        assert_eq!(rows[1].1, "text/plain");
        assert_eq!(rows[1].2, 1);
        assert_eq!(rows[1].3.as_deref(), Some("hi"));

        assert_eq!(rows[2].0, "alternative");
        assert_eq!(rows[2].1, "application/rtf");
        assert_eq!(rows[2].2, 2);
        assert_eq!(rows[2].3.as_deref(), Some("{\\rtf1 hi}"));
    }

    #[tokio::test]
    async fn list_representations_round_trips_role_ordinal_and_payload() {
        // Copy-back hydrates `PasteFormat::Preserve` clips through this read
        // API. Inserting an HTML+plain+RTF snapshot, then reading every row
        // back must return them in role-major (primary → plain_fallback →
        // alternative) order with payload, mime, and ordinal preserved so
        // the platform writer can republish the same multi-rep clip.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("list-rep-round-trip"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hi".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "application/rtf".to_owned(),
                    data: ClipboardData::Text("{\\rtf1 hi}".to_owned()),
                },
            ],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let store = SqliteStore::open_memory().unwrap();
        let id = store.insert(entry).await.unwrap();

        let reps = store.list_representations(id).await.unwrap();
        assert_eq!(reps.len(), 3);

        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "text/html");
        assert_eq!(reps[0].ordinal, 0);
        assert!(matches!(
            &reps[0].data,
            RepresentationDataRef::InlineText(text) if text == "<p>hi</p>"
        ));

        assert_eq!(reps[1].role, RepresentationRole::PlainFallback);
        assert_eq!(reps[1].mime_type, "text/plain");
        assert_eq!(reps[1].ordinal, 1);
        assert!(matches!(
            &reps[1].data,
            RepresentationDataRef::InlineText(text) if text == "hi"
        ));

        assert_eq!(reps[2].role, RepresentationRole::Alternative);
        assert_eq!(reps[2].mime_type, "application/rtf");
        assert_eq!(reps[2].ordinal, 2);
        assert!(matches!(
            &reps[2].data,
            RepresentationDataRef::InlineText(text) if text == "{\\rtf1 hi}"
        ));
    }

    #[tokio::test]
    async fn list_representations_returns_image_blob() {
        // Image bytes are persisted in `payload_blob`; the read path must
        // surface them as `RepresentationDataRef::DatabaseBlob` so the
        // platform writer can hand the raw bytes back to NSPasteboard
        // without a UTF-8 round-trip through `text_content`.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 1, 2, 3];
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("list-rep-image"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png_bytes.clone()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let store = SqliteStore::open_memory().unwrap();
        let id = store.insert(entry).await.unwrap();

        let reps = store.list_representations(id).await.unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "image/png");
        match &reps[0].data {
            RepresentationDataRef::DatabaseBlob(bytes) => assert_eq!(bytes, &png_bytes),
            other => panic!("expected DatabaseBlob, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_representations_decodes_file_paths_from_text_uri_list() {
        // File lists are persisted as a JSON array under the `text/uri-list`
        // mime; the read path must decode them back into a
        // `RepresentationDataRef::FilePaths` vector so the platform writer
        // can republish each path as a separate `NSPasteboardTypeFileURL`.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("list-rep-files"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec![
                    "/tmp/a.txt".to_owned(),
                    "/tmp/b.txt".to_owned(),
                ]),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let store = SqliteStore::open_memory().unwrap();
        let id = store.insert(entry).await.unwrap();

        let reps = store.list_representations(id).await.unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "text/uri-list");
        match &reps[0].data {
            RepresentationDataRef::FilePaths(paths) => {
                assert_eq!(
                    paths,
                    &vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()]
                );
            }
            other => panic!("expected FilePaths, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_representations_returns_empty_for_unknown_id() {
        let store = SqliteStore::open_memory().unwrap();
        let reps = store.list_representations(EntryId::new()).await.unwrap();
        assert!(reps.is_empty());
    }

    #[tokio::test]
    async fn list_representation_summaries_batches_across_entries() {
        // Palette refreshes ask for summaries of every visible row in a
        // single round-trip. Insert two multi-rep entries plus one
        // single-rep entry, query them as a batch, and confirm each id
        // gets back its own representations in role/ordinal order with no
        // payload bytes leaking through.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let mk = |seq: &str, reps: Vec<(&str, &str)>| {
            let snapshot = ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(seq),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: reps
                    .into_iter()
                    .map(|(mime, payload)| ClipboardRepresentation {
                        mime_type: mime.to_owned(),
                        data: ClipboardData::Text(payload.to_owned()),
                    })
                    .collect(),
            };
            EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry")
        };

        let store = SqliteStore::open_memory().unwrap();
        let id_a = store
            .insert(mk(
                "batch-a",
                vec![("text/html", "<p>a</p>"), ("text/plain", "a")],
            ))
            .await
            .unwrap();
        let id_b = store
            .insert(mk("batch-b", vec![("text/plain", "b")]))
            .await
            .unwrap();
        let id_c = store
            .insert(mk(
                "batch-c",
                vec![
                    ("text/html", "<i>c</i>"),
                    ("text/plain", "c"),
                    ("application/rtf", "{\\rtf1 c}"),
                ],
            ))
            .await
            .unwrap();

        let summaries = store
            .list_representation_summaries(&[id_a, id_b, id_c])
            .await
            .unwrap();
        assert_eq!(summaries.len(), 3);

        let a = summaries.get(&id_a).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].role, RepresentationRole::Primary);
        assert_eq!(a[0].mime_type, "text/html");
        assert_eq!(a[0].byte_count, "<p>a</p>".len() as u64);
        assert_eq!(a[1].role, RepresentationRole::PlainFallback);
        assert_eq!(a[1].mime_type, "text/plain");

        let b = summaries.get(&id_b).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].role, RepresentationRole::Primary);
        assert_eq!(b[0].mime_type, "text/plain");

        let c = summaries.get(&id_c).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].role, RepresentationRole::Primary);
        assert_eq!(c[1].role, RepresentationRole::PlainFallback);
        assert_eq!(c[2].role, RepresentationRole::Alternative);
        assert_eq!(c[2].mime_type, "application/rtf");
    }

    #[tokio::test]
    async fn list_representation_summaries_empty_input_returns_empty_map() {
        let store = SqliteStore::open_memory().unwrap();
        let summaries = store.list_representation_summaries(&[]).await.unwrap();
        assert!(summaries.is_empty());
    }

    #[tokio::test]
    async fn list_representation_summaries_skips_soft_deleted_entries() {
        // Mirror of `list_representations_skips_soft_deleted_entries` for
        // the batch path: a soft-deleted entry must not contribute rows
        // even when its id is supplied alongside live entries.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let store = SqliteStore::open_memory().unwrap();
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("batch-soft-delete"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("gone".to_owned()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let id = store.insert(entry).await.unwrap();
        store.mark_deleted(id).await.unwrap();

        let summaries = store.list_representation_summaries(&[id]).await.unwrap();
        assert!(!summaries.contains_key(&id));
    }

    /// Build a `FileList` entry carrying `paths` at the given sensitivity,
    /// using a distinct `display_text` so entries don't dedupe on insert.
    fn file_list_entry(paths: &[&str], sensitivity: Sensitivity) -> nagori_core::ClipboardEntry {
        use nagori_core::{ClipboardContent, FileListContent};
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        let mut entry = EntryFactory::from_content(
            ClipboardContent::FileList(FileListContent {
                display_text: paths.join("\n"),
                paths,
            }),
            None,
            None,
        );
        entry.sensitivity = sensitivity;
        entry
    }

    #[tokio::test]
    async fn list_file_path_sets_returns_paths_for_file_lists_only() {
        let store = SqliteStore::open_memory().unwrap();
        let files = vec!["/Users/example/Acme/a.pptx", "/Users/example/Acme/b.xlsx"];
        let file_id = store
            .insert(file_list_entry(&files, Sensitivity::Public))
            .await
            .unwrap();
        // A plain-text row is ignored even when its id rides along in the batch.
        let text_id = insert_text(&store, "not a file list").await;

        let sets = store
            .list_file_path_sets(&[file_id, text_id])
            .await
            .unwrap();
        assert_eq!(
            sets.get(&file_id),
            Some(&vec![
                "/Users/example/Acme/a.pptx".to_owned(),
                "/Users/example/Acme/b.xlsx".to_owned(),
            ])
        );
        assert!(!sets.contains_key(&text_id));
    }

    #[tokio::test]
    async fn list_file_path_sets_only_admits_public_and_unknown() {
        // The gate must mirror `is_text_safe_for_default_output`: a sensitive
        // file list must never leak its raw paths through this batch path.
        let store = SqliteStore::open_memory().unwrap();
        let public = store
            .insert(file_list_entry(&["/pub/a.pdf"], Sensitivity::Public))
            .await
            .unwrap();
        let unknown = store
            .insert(file_list_entry(&["/unk/b.pdf"], Sensitivity::Unknown))
            .await
            .unwrap();
        let private = store
            .insert(file_list_entry(&["/priv/c.pdf"], Sensitivity::Private))
            .await
            .unwrap();
        let secret = store
            .insert(file_list_entry(&["/sec/d.pdf"], Sensitivity::Secret))
            .await
            .unwrap();
        let blocked = store
            .insert(file_list_entry(&["/blk/e.pdf"], Sensitivity::Blocked))
            .await
            .unwrap();

        let sets = store
            .list_file_path_sets(&[public, unknown, private, secret, blocked])
            .await
            .unwrap();
        assert!(sets.contains_key(&public));
        assert!(sets.contains_key(&unknown));
        assert!(!sets.contains_key(&private));
        assert!(!sets.contains_key(&secret));
        assert!(!sets.contains_key(&blocked));
    }

    #[tokio::test]
    async fn list_file_path_sets_empty_input_returns_empty_map() {
        let store = SqliteStore::open_memory().unwrap();
        assert!(store.list_file_path_sets(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_file_path_sets_skips_soft_deleted_entries() {
        let store = SqliteStore::open_memory().unwrap();
        let id = store
            .insert(file_list_entry(&["/tmp/gone.txt"], Sensitivity::Public))
            .await
            .unwrap();
        store.mark_deleted(id).await.unwrap();
        assert!(
            !store
                .list_file_path_sets(&[id])
                .await
                .unwrap()
                .contains_key(&id)
        );
    }

    #[tokio::test]
    async fn list_representations_skips_soft_deleted_entries() {
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "soft delete me").await;
        store.mark_deleted(id).await.unwrap();
        let reps = store.list_representations(id).await.unwrap();
        assert!(reps.is_empty());
    }

    #[tokio::test]
    async fn trim_alternatives_drops_oversized_alts_before_insert() {
        // Mirror the capture pipeline's budget enforcement at the storage
        // boundary: feed an entry whose primary fits but whose alternatives
        // would blow past `max_total_bytes`, trim it, and confirm the only
        // rows that land in SQLite are the ones that survived the trim. The
        // recomputed `representation_set_hash` keeps dedupe honest about
        // what storage actually wrote.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            factory::compute_representation_set_hash,
        };

        let big_rtf = "{\\rtf1 ".to_owned() + &"a".repeat(2048) + "}";
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("trim-test"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hi".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "application/rtf".to_owned(),
                    data: ClipboardData::Text(big_rtf),
                },
            ],
        };
        let mut entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
        let trimmed = entry.trim_alternatives_to_budget(64);
        assert!(trimmed, "RTF alternative should be trimmed");
        entry.metadata.representation_set_hash = Some(compute_representation_set_hash(
            &entry.pending_representations,
        ));

        let store = SqliteStore::open_memory().unwrap();
        let id = store.insert(entry).await.unwrap();

        let conn = store.conn().unwrap();
        let mime_types: Vec<String> = conn
            .prepare(
                "SELECT mime_type FROM entry_representations
                 WHERE entry_id = ?1 ORDER BY ordinal ASC",
            )
            .unwrap()
            .query_map([id.to_string()], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(mime_types, vec!["text/html", "text/plain"]);
    }

    #[tokio::test]
    async fn duplicate_live_insert_does_not_duplicate_search_rows() {
        let store = SqliteStore::open_memory().unwrap();
        let first = insert_text(&store, "deduped once").await;
        let again = insert_text(&store, "deduped once").await;
        assert_eq!(first, again);

        let conn = store.conn().unwrap();
        for table in ["search_documents", "search_fts"] {
            let sql = format!("SELECT COUNT(*) FROM {table}");
            let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
            assert_eq!(count, 1, "{table} should only hold one row per live entry");
        }
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

    #[tokio::test]
    async fn enforce_total_bytes_includes_representation_payload() {
        // The retention budget must count every preserved representation
        // byte, not just the JSON envelope — otherwise a stream of large
        // images appears free and the policy never triggers eviction.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };

        let store = SqliteStore::open_memory().unwrap();

        let big_image_bytes = {
            let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
            bytes.resize(8 * 1024, 0xAB);
            bytes
        };
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash("big-image"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(big_image_bytes.clone()),
            }],
        };
        let image_entry =
            EntryFactory::from_snapshot(snapshot).expect("png snapshot should build entry");
        let image_id = store.insert(image_entry).await.unwrap();
        let _ = insert_text(&store, "small").await;

        // 1 KiB budget is well below the image's 8 KiB body, so the image
        // row should be evicted while the text-shaped row survives.
        let deleted = store.enforce_total_bytes(1024).await.unwrap();
        assert!(deleted >= 1, "image row should be soft-deleted");
        let fetched = store.get(image_id).await.unwrap();
        assert!(
            fetched.is_none(),
            "image row should be soft-deleted by byte budget"
        );

        let entry_payload = store.get_payload(image_id).await.unwrap();
        assert!(entry_payload.is_none());
        // After eviction the live representation count drops to the
        // surviving text entry's single row.
        let live_rep_count: i64 = {
            let conn = store.conn().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM entry_representations r
                 JOIN entries e ON e.id = r.entry_id
                 WHERE e.deleted_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            live_rep_count, 1,
            "only the surviving text row's representation should remain live"
        );
        let _ = big_image_bytes;
    }

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

    #[tokio::test]
    async fn get_payload_returns_none_for_text_entries() {
        // Text-shaped entries store their primary representation as inline
        // text, with no `payload_blob`. The preview path must therefore
        // return `None` for them so callers don't try to render the
        // representation row's `NULL` blob as image bytes.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "just text").await;

        let payload = store.get_payload(id).await.unwrap();
        assert!(payload.is_none());
    }
}
