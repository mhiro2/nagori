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
        let mut primary = Connection::open(path).map_err(storage_err)?;
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
            let conn = Connection::open(path).map_err(storage_err)?;
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
        let mut conn = Connection::open_in_memory().map_err(storage_err)?;
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
            .map_err(|err| AppError::storage_with(format!("blocking task failed: {err}"), err))?
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
            .map_err(|_| AppError::storage("search admission semaphore closed".to_owned()))?;

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
        .map_err(|err| AppError::storage_with(format!("blocking task failed: {err}"), err))?
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
            .map_err(storage_err)?;
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
    // that still hold the pre-deletion content; see ARCHITECTURE.md §19
    // for the at-rest posture and why app-level encryption is deferred.
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
    .map_err(storage_err)
}

pub(super) const MAX_READ_LIMIT: usize = 200;

pub(super) fn clamp_read_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_READ_LIMIT)
}

#[cfg(test)]
mod tests;
