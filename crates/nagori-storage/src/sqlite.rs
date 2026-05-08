use std::{
    fmt::Write as _,
    ops::{Deref, DerefMut},
    path::Path,
    str::FromStr,
    sync::{Arc, Condvar, Mutex},
};

use async_trait::async_trait;
use nagori_core::{
    AppError, AppSettings, AuditLog, ClipboardContent, ClipboardEntry, ContentHash, ContentKind,
    EntryId, EntryLifecycle, EntryMetadata, FtsCandidate, HashAlgorithm, NgramCandidate,
    PayloadRef, RecentOrder, Result, SearchCandidateProvider, SearchDocument, SearchFilters,
    SearchQuery, SearchRepository, SearchResult, SearchService, Sensitivity, SourceApp,
};
use nagori_search::{DefaultRanker, generate_ngrams, has_cjk, normalize_text};
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Number of physical `SQLite` connections we keep around for file-backed
/// stores.
///
/// The previous design held a single `Mutex<Connection>` and serialised every
/// read against every write. With WAL mode, `SQLite` already supports many
/// concurrent readers plus one writer on separate connections — so a small
/// pool lets the search fan-out (substring/FTS/ngram), preview hydration,
/// and capture writes proceed in parallel instead of queueing on one
/// process-wide mutex. Four is enough to soak up the hybrid search fan-out
/// (3 reads) plus an in-flight write without blocking, while keeping the
/// per-process file-descriptor cost bounded.
const POOL_CAPACITY: usize = 4;

#[derive(Clone)]
pub struct SqliteStore {
    pool: Arc<ConnPool>,
}

/// Bounded pool of `SQLite` connections.
///
/// `slots` holds whichever connections are currently idle. Acquirers pop the
/// front of the vector and return the connection on guard drop, notifying
/// any thread waiting in `available`. A pool with `capacity == 1` collapses
/// to today's single-`Mutex<Connection>` semantics — used for in-memory test
/// stores where each `Connection::open_in_memory` would create an entirely
/// separate database.
struct ConnPool {
    slots: Mutex<Vec<Connection>>,
    available: Condvar,
}

impl ConnPool {
    fn acquire(&self) -> Result<PooledConn<'_>> {
        let mut slots = self.slots.lock().map_err(|err| lock_err(&err))?;
        while slots.is_empty() {
            slots = self.available.wait(slots).map_err(|err| lock_err(&err))?;
        }
        let conn = slots.pop().expect("non-empty after wait");
        Ok(PooledConn {
            conn: Some(conn),
            pool: self,
        })
    }

    fn release(&self, conn: Connection) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.push(conn);
            self.available.notify_one();
        }
    }
}

/// RAII guard for a connection borrowed from a [`ConnPool`].
///
/// Drop returns the connection so callers don't need to release manually,
/// even on panic. The `Deref`/`DerefMut` impls make `PooledConn` a drop-in
/// replacement for the previous `MutexGuard<Connection>` callsites.
pub(crate) struct PooledConn<'a> {
    conn: Option<Connection>,
    pool: &'a ConnPool,
}

impl Deref for PooledConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection live")
    }
}

impl DerefMut for PooledConn<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection live")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut primary = Connection::open(path).map_err(|err| storage_err(&err))?;
        configure_connection(&primary)?;
        // Tighten the DB file to `0600` after open. SQLite creates the
        // file with the process umask, which can leave it world-readable
        // on a permissive shell. Clipboard history is sensitive — restrict
        // it to the owning user. The WAL/SHM sidecars get the same mask
        // as soon as SQLite touches them; we only need to fix the main
        // file we opened.
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
        })
    }

    pub fn open_memory() -> Result<Self> {
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
    async fn run_blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Self) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || f(store))
            .await
            .map_err(|err| AppError::Storage(format!("blocking task failed: {err}")))?
    }

    pub async fn set_pinned(&self, id: EntryId, pinned: bool) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let changed = {
                let conn = store.conn()?;
                conn.execute(
                    "UPDATE entries SET pinned = ?1, updated_at = ?2 WHERE id = ?3 AND deleted_at IS NULL",
                    params![bool_int(pinned), now, id.to_string()],
                )
                .map_err(|err| storage_err(&err))?
            };
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await
    }

    pub async fn increment_use_count(&self, id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let changed = {
                let conn = store.conn()?;
                conn.execute(
                    "UPDATE entries
                     SET use_count = use_count + 1, last_used_at = ?1, updated_at = ?1
                     WHERE id = ?2 AND deleted_at IS NULL",
                    params![now, id.to_string()],
                )
                .map_err(|err| storage_err(&err))?
            };
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await
    }

    pub async fn clear_older_than(&self, cutoff: OffsetDateTime) -> Result<usize> {
        self.run_blocking(move |store| {
            let cutoff = format_time(cutoff)?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries
                 SET deleted_at = ?1, updated_at = ?1
                 WHERE pinned = 0 AND deleted_at IS NULL AND created_at < ?2",
                    params![now, cutoff],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(changed)
        })
        .await
    }

    /// Soft-delete every non-pinned entry. Used by the desktop's
    /// `clear_on_quit` setting and the secondary "Clear history" hotkey.
    /// Pinned rows survive so users can keep curated snippets across the
    /// purge.
    pub async fn clear_non_pinned(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries
                 SET deleted_at = ?1, updated_at = ?1
                 WHERE pinned = 0 AND deleted_at IS NULL",
                    params![now],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(changed)
        })
        .await
    }

    pub async fn enforce_retention_count(&self, max_entries: usize) -> Result<usize> {
        if max_entries == 0 {
            return Ok(0);
        }
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries
                 SET deleted_at = ?1
                 WHERE deleted_at IS NULL
                   AND pinned = 0
                   AND id IN (
                       SELECT id FROM entries
                       WHERE deleted_at IS NULL AND pinned = 0
                       ORDER BY created_at DESC
                       LIMIT -1 OFFSET ?2
                   )",
                    params![now, max_entries as i64],
                )
                .map_err(|err| storage_err(&err))?;
            if changed > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(changed)
        })
        .await
    }

    pub async fn enforce_total_bytes(&self, max_total_bytes: u64) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let total_i64: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(content_json) + COALESCE(LENGTH(payload_blob), 0)), 0)
                     FROM entries
                     WHERE deleted_at IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|err| storage_err(&err))?;
            let mut total = u64::try_from(total_i64).map_err(|err| {
                AppError::Storage(format!("entry size total overflowed u64 conversion: {err}"))
            })?;
            if total <= max_total_bytes {
                tx.commit().map_err(|err| storage_err(&err))?;
                return Ok(0);
            }

            let candidates = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, LENGTH(content_json) + COALESCE(LENGTH(payload_blob), 0) AS entry_bytes
                         FROM entries
                         WHERE deleted_at IS NULL AND pinned = 0
                         ORDER BY created_at ASC, entry_bytes DESC",
                    )
                    .map_err(|err| storage_err(&err))?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|err| storage_err(&err))?;
                let rows = rows
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|err| storage_err(&err))?;
                rows.into_iter()
                    .map(|(id, bytes)| {
                        u64::try_from(bytes)
                            .map(|bytes| (id, bytes))
                            .map_err(|err| {
                                AppError::Storage(format!(
                                    "entry size overflowed u64 conversion: {err}"
                                ))
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };

            let now = format_time(OffsetDateTime::now_utc())?;
            let mut deleted = 0;
            for (id, bytes) in candidates {
                if total <= max_total_bytes {
                    break;
                }
                let changed = tx
                    .execute(
                        "UPDATE entries SET deleted_at = ?1, updated_at = ?1
                         WHERE id = ?2 AND deleted_at IS NULL AND pinned = 0",
                        params![now, id],
                    )
                    .map_err(|err| storage_err(&err))?;
                if changed > 0 {
                    deleted += changed;
                    total = total.saturating_sub(bytes);
                }
            }
            if deleted > 0 {
                prune_deleted_search_rows(&tx)?;
            }
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(deleted)
        })
        .await
    }

    /// Returns the inline payload blob and its mime type for an entry, or
    /// `None` if the row carries no blob (e.g. text/url entries) or has been
    /// soft-deleted.
    pub async fn get_payload(&self, id: EntryId) -> Result<Option<(Vec<u8>, String)>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT payload_blob, payload_mime FROM entries
                 WHERE id = ?1 AND deleted_at IS NULL",
                params![id.to_string()],
                |row| {
                    let blob: Option<Vec<u8>> = row.get(0)?;
                    let mime: Option<String> = row.get(1)?;
                    Ok(blob.zip(mime))
                },
            )
            .optional()
            .map_err(|err| storage_err(&err))
            .map(Option::flatten)
        })
        .await
    }

    pub async fn vacuum(&self) -> Result<()> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            conn.execute_batch("VACUUM")
                .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl AuditLog for SqliteStore {
    async fn record(
        &self,
        kind: &str,
        entry_id: Option<EntryId>,
        message: Option<&str>,
    ) -> Result<()> {
        let kind = kind.to_owned();
        let message = message.map(str::to_owned);
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let conn = store.conn()?;
            conn.execute(
                "INSERT INTO audit_events (id, event_kind, entry_id, message, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    kind,
                    entry_id.map(|id| id.to_string()),
                    message,
                    now,
                ],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl nagori_core::EntryRepository for SqliteStore {
    async fn insert(&self, entry: ClipboardEntry) -> Result<EntryId> {
        self.run_blocking(move |store| insert_entry_blocking(&store, &entry))
            .await
    }

    async fn get(&self, id: EntryId) -> Result<Option<ClipboardEntry>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language
                 FROM entries e
                 LEFT JOIN search_documents d ON d.entry_id = e.id
                 WHERE e.id = ?1 AND e.deleted_at IS NULL",
                params![id.to_string()],
                row_to_entry,
            )
            .optional()
            .map_err(|err| storage_err(&err))
        })
        .await
    }

    async fn update_metadata(&self, id: EntryId, metadata: EntryMetadata) -> Result<()> {
        self.run_blocking(move |store| {
            let changed = {
                let conn = store.conn()?;
                conn.execute(
                    "UPDATE entries
                     SET source_app_name = ?1, source_bundle_id = ?2, source_executable_path = ?3,
                         content_hash = ?4, use_count = ?5, updated_at = ?6, last_used_at = ?7
                     WHERE id = ?8 AND deleted_at IS NULL",
                    params![
                        metadata.source.as_ref().and_then(|s| s.name.as_deref()),
                        metadata
                            .source
                            .as_ref()
                            .and_then(|s| s.bundle_id.as_deref()),
                        metadata
                            .source
                            .as_ref()
                            .and_then(|s| s.executable_path.as_deref()),
                        metadata.content_hash.value,
                        metadata.use_count,
                        format_time(metadata.updated_at)?,
                        format_opt_time(metadata.last_used_at)?,
                        id.to_string(),
                    ],
                )
                .map_err(|err| storage_err(&err))?
            };
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn mark_deleted(&self, id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            let changed = tx
                .execute(
                    "UPDATE entries SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                    params![now, id.to_string()],
                )
                .map_err(|err| storage_err(&err))?;
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            delete_search_rows(&tx, &id.to_string())?;
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }

    async fn list_recent(&self, limit: usize) -> Result<Vec<ClipboardEntry>> {
        let limit = clamp_read_limit(limit);
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            fetch_recent_entries(
                &conn,
                &FilterFragment::default(),
                RecentOrder::ByRecency,
                limit as i64,
            )
        })
        .await
    }

    async fn list_pinned(&self) -> Result<Vec<ClipboardEntry>> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let mut stmt = conn
                .prepare(
                    "SELECT e.*, d.title, d.preview, d.normalized_text, d.language
                     FROM entries e
                     LEFT JOIN search_documents d ON d.entry_id = e.id
                     WHERE e.deleted_at IS NULL AND e.pinned = 1
                     ORDER BY e.updated_at DESC",
                )
                .map_err(|err| storage_err(&err))?;
            let entries = stmt
                .query_map([], row_to_entry)
                .map_err(|err| storage_err(&err))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|err| storage_err(&err))?;
            Ok(entries)
        })
        .await
    }
}

fn insert_entry_blocking(store: &SqliteStore, entry: &ClipboardEntry) -> Result<EntryId> {
    let requested_id = entry.id;
    let content_hash = entry.metadata.content_hash.value.clone();
    let updated_at = format_time(entry.metadata.updated_at)?;
    let mut doc = entry.search.clone();
    // Extract image bytes before serialising. `pending_bytes` is `#[serde(skip)]`
    // so the JSON body never grows by the blob size — the blob lives in
    // `entries.payload_blob` and is fetched lazily by the preview command.
    let (content_for_storage, payload_blob, payload_mime) = match &entry.content {
        ClipboardContent::Image(img) => {
            let mime = img.mime_type.clone();
            let bytes = img.pending_bytes.clone();
            let mut stripped = img.clone();
            stripped.pending_bytes = None;
            stripped.payload_ref = if bytes.is_some() {
                PayloadRef::DatabaseBlob(requested_id.to_string())
            } else {
                stripped.payload_ref.clone()
            };
            (ClipboardContent::Image(stripped), bytes, mime)
        }
        other => (other.clone(), None, None),
    };
    let mut conn = store.conn()?;
    let tx = conn.transaction().map_err(|err| storage_err(&err))?;
    // Resolve dedupe explicitly via SELECT-then-INSERT/UPDATE rather than
    // `INSERT ... ON CONFLICT(content_hash) WHERE deleted_at IS NULL`,
    // because conflict resolution against a partial unique index is
    // SQLite-version dependent.
    let existing = tx
        .query_row(
            "SELECT id FROM entries WHERE content_hash = ?1 AND deleted_at IS NULL",
            params![content_hash],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|err| storage_err(&err))?;
    let stored_id_str = if let Some(existing) = existing {
        // Refresh both `created_at` and `updated_at` so a re-copy of the
        // same content moves the entry back to the top of the recency
        // list — list/search ORDER BY `created_at DESC` and would otherwise
        // leave the duplicate buried in the original position.
        tx.execute(
            "UPDATE entries SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![updated_at, existing],
        )
        .map_err(|err| storage_err(&err))?;
        existing
    } else {
        tx.execute(
            "INSERT INTO entries (
                id, content_kind, text_content, content_json, source_app_name,
                source_bundle_id, source_executable_path, content_hash, sensitivity,
                pinned, archived, use_count, created_at, updated_at, last_used_at,
                expires_at, deleted_at, payload_blob, payload_mime
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                requested_id.to_string(),
                kind_to_str(entry.content_kind()),
                content_for_storage.plain_text(),
                serde_json::to_string(&content_for_storage).map_err(|err| json_err(&err))?,
                entry
                    .metadata
                    .source
                    .as_ref()
                    .and_then(|s| s.name.as_deref()),
                entry
                    .metadata
                    .source
                    .as_ref()
                    .and_then(|s| s.bundle_id.as_deref()),
                entry
                    .metadata
                    .source
                    .as_ref()
                    .and_then(|s| s.executable_path.as_deref()),
                content_hash,
                sensitivity_to_str(entry.sensitivity),
                bool_int(entry.lifecycle.pinned),
                bool_int(entry.lifecycle.archived),
                entry.metadata.use_count,
                format_time(entry.metadata.created_at)?,
                updated_at,
                format_opt_time(entry.metadata.last_used_at)?,
                format_opt_time(entry.lifecycle.expires_at)?,
                format_opt_time(entry.lifecycle.deleted_at)?,
                payload_blob,
                payload_mime,
            ],
        )
        .map_err(|err| storage_err(&err))?;
        requested_id.to_string()
    };
    let stored_id =
        EntryId::from_str(&stored_id_str).map_err(|err| AppError::Storage(err.to_string()))?;
    if stored_id != requested_id {
        doc.entry_id = stored_id;
    }
    upsert_document_blocking(&tx, &doc)?;
    tx.commit().map_err(|err| storage_err(&err))?;
    Ok(stored_id)
}

fn upsert_document_blocking(tx: &rusqlite::Transaction<'_>, doc: &SearchDocument) -> Result<()> {
    let entry_id = doc.entry_id.to_string();
    tx.execute(
        "INSERT INTO search_documents (entry_id, title, preview, normalized_text, language)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(entry_id) DO UPDATE SET
            title = excluded.title,
            preview = excluded.preview,
            normalized_text = excluded.normalized_text,
            language = excluded.language",
        params![
            entry_id,
            doc.title,
            doc.preview,
            doc.normalized_text,
            doc.language,
        ],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute(
        "DELETE FROM search_fts WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute(
        "INSERT INTO search_fts (entry_id, title, preview, normalized_text)
         SELECT entry_id, title, preview, normalized_text
         FROM search_documents WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute("DELETE FROM ngrams WHERE entry_id = ?1", params![entry_id])
        .map_err(|err| storage_err(&err))?;
    let mut stmt = tx
        .prepare("INSERT OR IGNORE INTO ngrams (gram, entry_id, position) VALUES (?1, ?2, ?3)")
        .map_err(|err| storage_err(&err))?;
    for (position, gram) in generate_ngrams(&doc.normalized_text).iter().enumerate() {
        stmt.execute(params![gram, entry_id, position as i64])
            .map_err(|err| storage_err(&err))?;
    }
    Ok(())
}

fn delete_search_rows(tx: &rusqlite::Transaction<'_>, entry_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM search_documents WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute(
        "DELETE FROM search_fts WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute("DELETE FROM ngrams WHERE entry_id = ?1", params![entry_id])
        .map_err(|err| storage_err(&err))?;
    Ok(())
}

#[async_trait]
impl SearchRepository for SqliteStore {
    async fn upsert_document(&self, doc: SearchDocument) -> Result<()> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            upsert_document_blocking(&tx, &doc)?;
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }

    async fn delete_document(&self, entry_id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            delete_search_rows(&tx, &entry_id.to_string())?;
            tx.commit().map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}

impl SqliteStore {
    /// Convenience wrapper that runs a [`SearchQuery`] through the canonical
    /// [`SearchService`] using the `nagori-search` ranker. Kept on the store
    /// so existing callers (CLI, daemon, Tauri, benches) don't have to wire
    /// the service up themselves.
    pub async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        SearchService::new(self, &DefaultRanker).search(query).await
    }
}

#[async_trait]
impl SearchCandidateProvider for SqliteStore {
    async fn recent_entries(
        &self,
        filters: &SearchFilters,
        order: RecentOrder,
        limit: usize,
    ) -> Result<Vec<ClipboardEntry>> {
        let filter = build_filter_fragment(filters);
        let limit_i64 = limit as i64;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            fetch_recent_entries(&conn, &filter, order, limit_i64)
        })
        .await
    }

    async fn substring_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
        bounded: bool,
    ) -> Result<Vec<ClipboardEntry>> {
        let filter = build_filter_fragment(filters);
        let like = format!("%{}%", escape_like(normalized));
        let limit_i64 = limit as i64;
        let scan_window = SUBSTRING_SCAN_WINDOW;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // LIKE can't hit a secondary index for `%term%`, so for the hybrid
            // path we cap the candidate set to the most recent
            // `SUBSTRING_SCAN_WINDOW` live entries via a CTE. The composite
            // `idx_entries_recent_live(pinned DESC, created_at DESC)` lets
            // the planner walk that inner subquery as an index range scan
            // and stop after the limit, so per-keystroke tail latency stays
            // bounded as history grows. FTS / ngram cover older rows in the
            // hybrid plan, so the window doesn't drop reachable matches.
            //
            // For an explicit `Exact` query (`bounded == false`) substring
            // is the only branch, so we walk the full live corpus to avoid
            // silently hiding old matches outside the window.
            let sql = if bounded {
                format!(
                    "WITH recent_live AS (
                         SELECT id FROM entries
                         WHERE deleted_at IS NULL AND sensitivity != 'blocked'
                         ORDER BY pinned DESC, created_at DESC
                         LIMIT ?
                     )
                     SELECT DISTINCT e.*, d.title, d.preview, d.normalized_text, d.language
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     JOIN recent_live r ON r.id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity != 'blocked'
                       AND d.normalized_text LIKE ? ESCAPE '\\'
                       {extra}
                     ORDER BY e.pinned DESC, e.created_at DESC
                     LIMIT ?",
                    extra = filter.sql,
                )
            } else {
                format!(
                    "SELECT e.*, d.title, d.preview, d.normalized_text, d.language
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity != 'blocked'
                       AND d.normalized_text LIKE ? ESCAPE '\\'
                       {extra}
                     ORDER BY e.pinned DESC, e.created_at DESC
                     LIMIT ?",
                    extra = filter.sql,
                )
            };
            let mut stmt = conn.prepare(&sql).map_err(|err| storage_err(&err))?;
            let mut bound: Vec<&dyn ToSql> = Vec::new();
            if bounded {
                bound.push(&scan_window);
            }
            bound.push(&like);
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            let mut entries = Vec::new();
            for row in stmt
                .query_map(rusqlite::params_from_iter(bound), row_to_entry)
                .map_err(|err| storage_err(&err))?
            {
                entries.push(row.map_err(|err| storage_err(&err))?);
            }
            Ok(entries)
        })
        .await
    }

    async fn fulltext_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<FtsCandidate>> {
        let fts = fts_query(normalized);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let filter = build_filter_fragment(filters);
        let limit_i64 = limit as i64;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let sql = format!(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language,
                        bm25(search_fts) AS fts_score
                 FROM search_fts f
                 JOIN entries e ON e.id = f.entry_id
                 JOIN search_documents d ON d.entry_id = e.id
                 WHERE search_fts MATCH ?
                   AND e.deleted_at IS NULL
                   AND e.sensitivity != 'blocked'
                   {extra}
                 ORDER BY fts_score
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare(&sql).map_err(|err| storage_err(&err))?;
            let mut bound: Vec<&dyn ToSql> = vec![&fts];
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    let score: f64 = row.get("fts_score").unwrap_or(0.0);
                    let entry = row_to_entry(row)?;
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(FtsCandidate {
                        entry,
                        fts_score: score as f32,
                    })
                })
                .map_err(|err| storage_err(&err))?;
            let mut hits = Vec::new();
            for row in rows {
                hits.push(row.map_err(|err| storage_err(&err))?);
            }
            Ok(hits)
        })
        .await
    }

    async fn ngram_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<NgramCandidate>> {
        let query_grams = generate_ngrams(normalized);
        // Ngram fan-out only pays off for CJK or very short queries — long
        // ASCII queries return huge candidate sets that LIKE/FTS already
        // cover, so we shortcut to an empty result instead of doing the work.
        if query_grams.is_empty() || !(has_cjk(normalized) || normalized.chars().count() <= 8) {
            return Ok(Vec::new());
        }
        let filter = build_filter_fragment(filters);
        let limit_i64 = limit as i64;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let placeholders = std::iter::repeat_n("?", query_grams.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language,
                        COUNT(DISTINCT n.gram) AS hits
                 FROM ngrams n
                 JOIN entries e ON e.id = n.entry_id
                 JOIN search_documents d ON d.entry_id = e.id
                 WHERE n.gram IN ({placeholders})
                   AND e.deleted_at IS NULL
                   AND e.sensitivity != 'blocked'
                   {extra}
                 GROUP BY e.id
                 ORDER BY hits DESC, e.created_at DESC
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare(&sql).map_err(|err| storage_err(&err))?;
            let mut bound: Vec<&dyn ToSql> = query_grams.iter().map(|g| g as &dyn ToSql).collect();
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            #[allow(clippy::cast_precision_loss)]
            let total = query_grams.len() as f32;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    let hits: i64 = row.get("hits").unwrap_or(0);
                    let entry = row_to_entry(row)?;
                    Ok((entry, hits))
                })
                .map_err(|err| storage_err(&err))?;
            let mut out = Vec::new();
            for row in rows {
                let (entry, hits) = row.map_err(|err| storage_err(&err))?;
                #[allow(clippy::cast_precision_loss)]
                let overlap = (hits as f32 / total).clamp(0.0, 1.0);
                out.push(NgramCandidate {
                    entry,
                    ngram_overlap: overlap,
                });
            }
            Ok(out)
        })
        .await
    }
}

fn fetch_recent_entries(
    conn: &Connection,
    filter: &FilterFragment,
    order: RecentOrder,
    limit: i64,
) -> Result<Vec<ClipboardEntry>> {
    let order_sql = match order {
        RecentOrder::ByRecency => "ORDER BY e.created_at DESC",
        RecentOrder::ByUseCount => {
            "ORDER BY e.use_count DESC, COALESCE(e.last_used_at, e.created_at) DESC, e.created_at DESC"
        }
        RecentOrder::PinnedFirstThenRecency => "ORDER BY e.pinned DESC, e.created_at DESC",
    };
    let sql = format!(
        "SELECT e.*, d.title, d.preview, d.normalized_text, d.language
         FROM entries e
         LEFT JOIN search_documents d ON d.entry_id = e.id
         WHERE e.deleted_at IS NULL
           AND e.sensitivity != 'blocked'
           {extra}
         {order_sql}
         LIMIT ?",
        extra = filter.sql,
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| storage_err(&err))?;
    let mut bound: Vec<&dyn ToSql> = filter.params.iter().map(|p| &**p as &dyn ToSql).collect();
    bound.push(&limit);
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound), row_to_entry)
        .map_err(|err| storage_err(&err))?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row.map_err(|err| storage_err(&err))?);
    }
    Ok(entries)
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(unix)]
fn harden_db_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|err| storage_err_io(&err))
}

#[cfg(not(unix))]
fn harden_db_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn storage_err_io(err: &std::io::Error) -> AppError {
    AppError::Storage(err.to_string())
}

/// Create `dir` with `0o700` perms on Unix so the parent isn't world-traversable.
///
/// Used for the clipboard DB and IPC socket directories. Idempotent — if the
/// directory already exists, the mode is reset to `0o700`.
pub fn ensure_private_directory(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|err| AppError::Storage(err.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| AppError::Storage(err.to_string()))?;
    }
    Ok(())
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|err| storage_err(&err))
}

const MAX_READ_LIMIT: usize = 200;

/// Recent-entry window the substring (LIKE) candidate scanner is restricted
/// to. The substring path can't hit a secondary index for `%term%`, so we
/// trade unbounded recall on very old rows for predictable per-keystroke
/// latency: FTS and ngram still see the entire corpus, this branch only
/// backstops them on the recent window where exact substring matches are
/// most useful.
const SUBSTRING_SCAN_WINDOW: i64 = 5_000;

fn clamp_read_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_READ_LIMIT)
}

/// Tauri global-shortcut format: zero or more modifiers, `+`-separated, then
/// exactly one key segment. We can't fully verify the OS will accept the
/// final binding (that depends on the Tauri parser at register time), but
/// catching the obvious shape mistakes here means a typo'd hotkey from the
/// settings UI never lands in storage and silently disables the feature
/// after the next restart.
fn validate_hotkey(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(
            "global_hotkey must not be empty".to_owned(),
        ));
    }
    if trimmed != raw {
        return Err(AppError::InvalidInput(
            "global_hotkey must not have leading/trailing whitespace".to_owned(),
        ));
    }
    let segments: Vec<&str> = trimmed.split('+').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(AppError::InvalidInput(
            "global_hotkey must not contain empty `+` segments".to_owned(),
        ));
    }
    let (key, mods) = segments.split_last().expect("non-empty after trim check");
    let mut seen = std::collections::HashSet::new();
    for m in mods {
        let canonical = canonical_modifier(m).ok_or_else(|| {
            AppError::InvalidInput(format!("global_hotkey: unknown modifier `{m}`"))
        })?;
        if !seen.insert(canonical) {
            return Err(AppError::InvalidInput(format!(
                "global_hotkey: duplicate modifier `{m}`"
            )));
        }
    }
    if canonical_modifier(key).is_some() {
        return Err(AppError::InvalidInput(
            "global_hotkey must end with a non-modifier key".to_owned(),
        ));
    }
    if !is_valid_hotkey_key(key) {
        return Err(AppError::InvalidInput(format!(
            "global_hotkey: invalid key `{key}`"
        )));
    }
    Ok(())
}

fn canonical_modifier(token: &str) -> Option<&'static str> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "meta" | "win" | "windows" => Some("super"),
        "ctrl" | "control" => Some("ctrl"),
        "cmdorctrl" | "commandorcontrol" => Some("cmdorctrl"),
        "alt" | "option" | "opt" => Some("alt"),
        "shift" => Some("shift"),
        _ => None,
    }
}

fn is_valid_hotkey_key(key: &str) -> bool {
    // Single printable ASCII char (letter/digit/punct), or a named key from
    // the known whitelist. This mirrors what `tauri-plugin-global-shortcut`
    // accepts on macOS today; new tokens can be added here as needed.
    if key.chars().count() == 1 {
        let c = key.chars().next().expect("len-checked above");
        return c.is_ascii_alphanumeric() || "`-=[]\\;',./".contains(c);
    }
    let upper = key.to_ascii_uppercase();
    if upper.starts_with('F')
        && upper.len() <= 3
        && upper[1..].chars().all(|c| c.is_ascii_digit())
        && let Ok(n) = upper[1..].parse::<u32>()
    {
        return (1..=24).contains(&n);
    }
    matches!(
        upper.as_str(),
        "SPACE"
            | "ENTER"
            | "RETURN"
            | "ESC"
            | "ESCAPE"
            | "TAB"
            | "BACKSPACE"
            | "DELETE"
            | "INSERT"
            | "UP"
            | "DOWN"
            | "LEFT"
            | "RIGHT"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "CAPSLOCK"
            | "NUMLOCK"
            | "SCROLLLOCK"
            | "PRINTSCREEN"
    )
}

fn validate_settings(settings: &AppSettings) -> Result<()> {
    validate_hotkey(&settings.global_hotkey)?;
    settings.validate()
}

fn prune_deleted_search_rows(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM search_documents
         WHERE entry_id IN (SELECT id FROM entries WHERE deleted_at IS NOT NULL)",
        [],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute(
        "DELETE FROM search_fts
         WHERE entry_id NOT IN (SELECT id FROM entries WHERE deleted_at IS NULL)",
        [],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute(
        "DELETE FROM ngrams
         WHERE entry_id NOT IN (SELECT id FROM entries WHERE deleted_at IS NULL)",
        [],
    )
    .map_err(|err| storage_err(&err))?;
    Ok(())
}

#[derive(Default)]
struct FilterFragment {
    sql: String,
    // `Send + Sync` is required because `FilterFragment` is built outside
    // `run_blocking` and then moved into the blocking closure where the
    // actual SQL is executed. Without these bounds the closure can't cross
    // tokio's thread boundary.
    params: Vec<Box<dyn ToSql + Send + Sync>>,
}

fn build_filter_fragment(filters: &SearchFilters) -> FilterFragment {
    let mut fragment = FilterFragment::default();
    if !filters.kinds.is_empty() {
        let placeholders = std::iter::repeat_n("?", filters.kinds.len())
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(fragment.sql, " AND e.content_kind IN ({placeholders})");
        for kind in &filters.kinds {
            fragment
                .params
                .push(Box::new(kind_to_str(*kind).to_owned()));
        }
    }
    if filters.pinned_only {
        fragment.sql.push_str(" AND e.pinned = 1");
    }
    if let Some(source) = &filters.source_app {
        fragment
            .sql
            .push_str(" AND (e.source_bundle_id = ? OR e.source_app_name = ?)");
        fragment.params.push(Box::new(source.clone()));
        fragment.params.push(Box::new(source.clone()));
    }
    if let Some(after) = filters.created_after {
        fragment.sql.push_str(" AND e.created_at >= ?");
        fragment
            .params
            .push(Box::new(after.format(&Rfc3339).unwrap_or_default()));
    }
    if let Some(before) = filters.created_before {
        fragment.sql.push_str(" AND e.created_at <= ?");
        fragment
            .params
            .push(Box::new(before.format(&Rfc3339).unwrap_or_default()));
    }
    fragment
}

#[async_trait]
impl nagori_core::SettingsRepository for SqliteStore {
    async fn get_settings(&self) -> Result<AppSettings> {
        self.run_blocking(|store| {
            let conn = store.conn()?;
            let value: Option<String> = conn
                .query_row("SELECT value FROM settings WHERE key = 'app'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|err| storage_err(&err))?;
            let settings: AppSettings = match value {
                Some(value) => serde_json::from_str(&value).map_err(|err| json_err(&err))?,
                None => return Ok(AppSettings::default()),
            };
            // Hand-edited or downgraded rows can carry out-of-range values
            // (`paste_delay_ms = u64::MAX`, `palette_row_count = 0`, …) that
            // wedge the consumer. Validate on every load — the same gate
            // `save_settings` enforces — so a corrupt row surfaces loudly
            // instead of silently freezing paste or breaking the palette.
            settings.validate()?;
            Ok(settings)
        })
        .await
    }

    async fn save_settings(&self, settings: AppSettings) -> Result<()> {
        validate_settings(&settings)?;
        for pattern in &settings.regex_denylist {
            regex::Regex::new(pattern).map_err(|err| {
                AppError::InvalidInput(format!("invalid regex_denylist entry {pattern:?}: {err}"))
            })?;
        }
        self.run_blocking(move |store| {
            let value = serde_json::to_string_pretty(&settings).map_err(|err| json_err(&err))?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let conn = store.conn()?;
            conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ('app', ?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![value, now],
            )
            .map_err(|err| storage_err(&err))?;
            Ok(())
        })
        .await
    }
}

/// Ordered list of schema migrations.
///
/// Each entry is `(target_version, sql)`. `target_version` must be strictly
/// greater than the previous entry's, contiguous, and monotonic — never
/// renumber, never reorder, never edit a published migration. To change
/// existing schema, append a new migration with a higher version that
/// performs the alter step. `run_migrations` plays each pending migration
/// in its own transaction and bumps `user_version` so partial application
/// can never leave the DB at a half-migrated state.
const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, SCHEMA_V2)];

/// Highest schema version supported by this binary. A DB whose
/// `user_version` already exceeds this is from a newer build and we refuse
/// to run against it rather than silently downgrade.
const SCHEMA_VERSION: i64 = const_max_version(MIGRATIONS);

const fn const_max_version(list: &[(i64, &str)]) -> i64 {
    let mut idx = 0;
    let mut max = 0;
    while idx < list.len() {
        if list[idx].0 > max {
            max = list[idx].0;
        }
        idx += 1;
    }
    max
}

fn run_migrations(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("SELECT user_version FROM pragma_user_version", [], |row| {
            row.get(0)
        })
        .map_err(|err| storage_err(&err))?;
    if current > SCHEMA_VERSION {
        return Err(AppError::Storage(format!(
            "database schema version {current} is newer than this build supports ({SCHEMA_VERSION}); refusing to open",
        )));
    }
    let mut last_applied = current;
    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        if *version != last_applied + 1 {
            return Err(AppError::Storage(format!(
                "schema migrations are non-contiguous: jumped from {last_applied} to {version}",
            )));
        }
        let tx = conn.transaction().map_err(|err| storage_err(&err))?;
        tx.execute_batch(sql).map_err(|err| storage_err(&err))?;
        // `PRAGMA user_version = ?` is parsed as a literal — the value
        // must be embedded in the statement text, not bound. Inlining a
        // server-controlled integer here is safe.
        tx.execute_batch(&format!("PRAGMA user_version = {version};"))
            .map_err(|err| storage_err(&err))?;
        tx.commit().map_err(|err| storage_err(&err))?;
        last_applied = *version;
    }
    Ok(())
}

const SCHEMA_V1: &str = r"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS entries (
    id TEXT PRIMARY KEY,
    content_kind TEXT NOT NULL,
    text_content TEXT,
    content_json TEXT NOT NULL,
    payload_ref TEXT,
    source_app_name TEXT,
    source_bundle_id TEXT,
    source_executable_path TEXT,
    content_hash TEXT NOT NULL,
    sensitivity TEXT NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    use_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    expires_at TEXT,
    deleted_at TEXT,
    payload_blob BLOB,
    payload_mime TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_content_hash
ON entries(content_hash)
WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_entries_created_at ON entries(created_at);
CREATE INDEX IF NOT EXISTS idx_entries_pinned ON entries(pinned);

CREATE TABLE IF NOT EXISTS search_documents (
    entry_id TEXT PRIMARY KEY,
    title TEXT,
    preview TEXT NOT NULL,
    normalized_text TEXT NOT NULL,
    language TEXT,
    FOREIGN KEY(entry_id) REFERENCES entries(id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
    entry_id UNINDEXED,
    title,
    preview,
    normalized_text,
    tokenize = 'unicode61'
);

CREATE TABLE IF NOT EXISTS ngrams (
    gram TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (gram, entry_id, position)
);

CREATE INDEX IF NOT EXISTS idx_ngrams_entry_id ON ngrams(entry_id);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    event_kind TEXT NOT NULL,
    entry_id TEXT,
    message TEXT,
    created_at TEXT NOT NULL
);

-- Composite indexes for the substring / recent_entries / ngram hot paths.
--
-- `recent_entries` and `substring_candidates` both filter on
-- `deleted_at IS NULL AND sensitivity != 'blocked'` and order by
-- `pinned DESC, created_at DESC`. Without a covering composite the planner
-- either sorts a tablescan (`idx_entries_created_at` doesn't include
-- `pinned`) or sorts the result of `idx_entries_pinned`. A partial index
-- over the live, non-blocked rows in the same order lets SQLite walk the
-- index forward and stop after `LIMIT`. The partial predicate also keeps
-- the index from carrying soft-deleted history we never query.
--
-- `idx_ngrams_gram_entry` is for the ngram fan-out: the candidate query
-- filters `WHERE n.gram IN (...)` then groups by `entry_id`, so a
-- `(gram, entry_id)` composite gets us straight to the matching rows
-- without scanning each gram's full posting list.
CREATE INDEX IF NOT EXISTS idx_entries_recent_live
    ON entries(pinned DESC, created_at DESC)
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

CREATE INDEX IF NOT EXISTS idx_ngrams_gram_entry
    ON ngrams(gram, entry_id);
";

/// Backfill the substring / ngram hot-path indexes for databases that were
/// created before they shipped.
///
/// Fresh installs already get these via `SCHEMA_V1`; the `IF NOT EXISTS`
/// guards make this migration a no-op for them. Pre-existing v1 databases
/// (the ones the storage rewrite was actually motivated by — large
/// histories that the bounded substring scan and the gram-entry composite
/// were designed to keep snappy) need a separate migration step because
/// the migration runner only re-runs scripts whose `target_version` is
/// strictly greater than the stored `user_version`.
const SCHEMA_V2: &str = r"
CREATE INDEX IF NOT EXISTS idx_entries_recent_live
    ON entries(pinned DESC, created_at DESC)
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

CREATE INDEX IF NOT EXISTS idx_ngrams_gram_entry
    ON ngrams(gram, entry_id);
";

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<ClipboardEntry> {
    let id = EntryId::from_str(&row.get::<_, String>("id")?)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let content_json: String = row.get("content_json")?;
    let content: ClipboardContent = serde_json::from_str(&content_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let hash = ContentHash {
        algorithm: HashAlgorithm::Sha256,
        value: row.get("content_hash")?,
    };
    let source = {
        let name: Option<String> = row.get("source_app_name")?;
        let bundle_id: Option<String> = row.get("source_bundle_id")?;
        let executable_path: Option<String> = row.get("source_executable_path")?;
        (name.is_some() || bundle_id.is_some() || executable_path.is_some()).then_some(SourceApp {
            name,
            bundle_id,
            executable_path,
        })
    };
    let metadata = EntryMetadata {
        created_at: parse_time(&row.get::<_, String>("created_at")?)?,
        updated_at: parse_time(&row.get::<_, String>("updated_at")?)?,
        last_used_at: parse_opt_time(row.get("last_used_at")?)?,
        use_count: row.get::<_, u32>("use_count")?,
        source,
        content_hash: hash,
    };
    let search = SearchDocument {
        entry_id: id,
        title: row.get("title").unwrap_or(None),
        preview: row.get("preview").unwrap_or_else(|_| {
            nagori_core::make_preview(content.plain_text().unwrap_or_default(), 180)
        }),
        normalized_text: row
            .get("normalized_text")
            .unwrap_or_else(|_| normalize_text(content.plain_text().unwrap_or_default())),
        tokens: Vec::new(),
        language: row.get("language").unwrap_or(None),
    };
    Ok(ClipboardEntry {
        id,
        content,
        metadata,
        search,
        sensitivity: parse_sensitivity_strict(&row.get::<_, String>("sensitivity")?)?,
        lifecycle: EntryLifecycle {
            pinned: row.get::<_, i64>("pinned")? != 0,
            archived: row.get::<_, i64>("archived")? != 0,
            deleted_at: parse_opt_time(row.get("deleted_at")?)?,
            expires_at: parse_opt_time(row.get("expires_at")?)?,
        },
    })
}

const fn kind_to_str(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Text => "text",
        ContentKind::Url => "url",
        ContentKind::Code => "code",
        ContentKind::Image => "image",
        ContentKind::FileList => "file_list",
        ContentKind::RichText => "rich_text",
        ContentKind::Unknown => "unknown",
    }
}

const fn sensitivity_to_str(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Unknown => "unknown",
        Sensitivity::Public => "public",
        Sensitivity::Private => "private",
        Sensitivity::Secret => "secret",
        Sensitivity::Blocked => "blocked",
    }
}

/// Strict variant for `row_to_entry`. Refuses to coerce a foreign sensitivity
/// label into `Unknown` — a stray value in the DB column means either the
/// schema has drifted ahead of this build (in which case we should refuse
/// to open instead of misclassifying secret rows as `Unknown`) or the column
/// has been tampered with. Either way, returning an error surfaces the issue
/// instead of silently downgrading the sensitivity guard.
fn parse_sensitivity_strict(value: &str) -> rusqlite::Result<Sensitivity> {
    match value {
        "public" => Ok(Sensitivity::Public),
        "private" => Ok(Sensitivity::Private),
        "secret" => Ok(Sensitivity::Secret),
        "blocked" => Ok(Sensitivity::Blocked),
        "unknown" => Ok(Sensitivity::Unknown),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(AppError::Storage(format!(
                "unknown sensitivity label in DB row: {other:?}"
            ))),
        )),
    }
}

fn bool_int(value: bool) -> i64 {
    i64::from(value)
}

/// Render the user's normalized query into an FTS5 MATCH expression.
///
/// Each surviving token is wrapped in `"..."` so FTS5 treats it as a
/// phrase string rather than a bareword that could parse as an operator.
/// We *also* split on the FTS5 metacharacters `(`, `)`, `:`, `*`, and `"`
/// in addition to whitespace: a bareword like `foo:bar` would tokenize
/// fine inside quotes, but a query consisting solely of those chars
/// (e.g. `(` or `:`) previously produced `"("` — a phrase that the
/// tokenizer collapses to zero tokens, raising an FTS5 syntax error at
/// runtime. Stripping them at split time keeps the resulting expression
/// well-formed and removes any path for an unmatched `"` or
/// column-filter `:` to leak through unescaped. Empty fragments are
/// discarded so a query of pure punctuation returns the empty string,
/// which the caller treats as "no FTS candidates".
fn fts_query(query: &str) -> String {
    query
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ':' | '*' | '"'))
        .filter(|part| !part.is_empty())
        .map(|part| format!("\"{part}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|err| AppError::Storage(err.to_string()))
}

fn format_opt_time(value: Option<OffsetDateTime>) -> Result<Option<String>> {
    value.map(format_time).transpose()
}

fn parse_time(value: &str) -> rusqlite::Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

// Callers receive `Option<String>` directly from `row.get`; taking ownership avoids extra rebinding.
#[allow(clippy::needless_pass_by_value)]
fn parse_opt_time(value: Option<String>) -> rusqlite::Result<Option<OffsetDateTime>> {
    value.as_deref().map(parse_time).transpose()
}

fn storage_err(err: &rusqlite::Error) -> AppError {
    AppError::Storage(err.to_string())
}

fn json_err(err: &serde_json::Error) -> AppError {
    AppError::Storage(err.to_string())
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Storage(err.to_string())
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        ContentKind, EntryFactory, EntryRepository, RankReason, SearchFilters, SearchMode,
        SearchQuery,
    };
    use nagori_search::normalize_text;

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

    #[test]
    fn validate_hotkey_accepts_common_shapes() {
        for ok in [
            "CmdOrCtrl+Shift+V",
            "Cmd+V",
            "Ctrl+Alt+P",
            "Shift+F12",
            "Alt+Space",
            "CmdOrCtrl+Enter",
        ] {
            validate_hotkey(ok).unwrap_or_else(|err| panic!("expected `{ok}` to validate: {err}"));
        }
    }

    #[test]
    fn validate_hotkey_rejects_bad_shapes() {
        for bad in [
            "",
            "  ",
            "Cmd",               // modifier only
            "Cmd+",              // empty key
            "+Cmd+V",            // empty leading segment
            "Cmd++V",            // empty middle segment
            "Cmd+Foo+V",         // unknown modifier
            "Cmd+Shift+Shift+V", // duplicate modifier (after canonicalization)
            "Cmd+F25",           // function key out of range
            "Cmd+Hyperspace",    // unknown named key
            " Cmd+V",            // leading whitespace
            "Cmd+V ",            // trailing whitespace
        ] {
            assert!(
                validate_hotkey(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
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

        query.mode = SearchMode::Auto;
        let auto = store.search(query).await.unwrap();
        assert!(!auto.is_empty());
    }

    #[tokio::test]
    async fn exact_substring_walks_full_corpus_unbounded() {
        // Regression: an earlier iteration capped the substring CTE to the
        // most recent SUBSTRING_SCAN_WINDOW rows for *all* plans, which
        // silently dropped exact matches outside the window. The Exact
        // plan must always see the full live corpus because nothing else
        // (FTS / ngram) backstops it.
        use nagori_core::SearchCandidateProvider;
        let store = SqliteStore::open_memory().unwrap();
        let _old = insert_text(&store, "needle in a haystack").await;
        for idx in 0..20 {
            let _ = insert_text(&store, &format!("filler {idx}")).await;
        }
        let bounded = store
            .substring_candidates("needle", &SearchFilters::default(), 10, true)
            .await
            .unwrap();
        let unbounded = store
            .substring_candidates("needle", &SearchFilters::default(), 10, false)
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
            sequence: ClipboardSequence("img-1".to_owned()),
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
        // live in `entries.payload_blob`, not inside `content_json`.
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
}
