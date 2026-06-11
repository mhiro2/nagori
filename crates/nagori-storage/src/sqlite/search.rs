use std::fmt::Write as _;

use async_trait::async_trait;
use nagori_core::{
    ClipboardEntry, EntryId, FtsCandidate, NgramCandidate, NgramQueryMode, RecentOrder, Result,
    SearchCandidate, SearchCandidateProvider, SearchDocument, SearchFilters, SearchQuery,
    SearchRepository, SearchResult, SearchService,
};
use nagori_search::{
    DefaultRanker, MAX_NGRAM_INPUT_CHARS, generate_document_ngrams, generate_query_ngrams, has_cjk,
    ngram_input_was_truncated,
};
use rusqlite::{Connection, OptionalExtension, ToSql, params};
use tokio_util::sync::CancellationToken;

use super::SqliteStore;
use super::convert::{
    format_time, fts_query, kind_to_str, row_to_candidate, row_to_entry, storage_err,
};

/// Projection column list shared by every candidate query, consumed by
/// [`row_to_candidate`]. The `CASE` keeps `content_json` out of the result set
/// for ordinary text rows: it only travels when the row is an image (the
/// result surfaces pixel dimensions) or its `search_documents` join is missing
/// (the preview / normalized-text fallback needs the body), so the search hot
/// path never reads or deserializes multi-hundred-KiB bodies per candidate.
const CANDIDATE_COLUMNS: &str = "e.id, e.content_kind, e.created_at, e.use_count, e.pinned,
            e.sensitivity, e.source_app_name, d.preview, d.normalized_text, d.language,
            CASE WHEN e.content_kind = 'image' OR d.entry_id IS NULL
                 THEN e.content_json END AS candidate_content_json";

/// Current ngram-generator revision. Bump whenever
/// [`generate_document_ngrams`]'s output for a given `normalized_text` changes
/// (kana folding, Han 1-grams, …). [`upsert_document_blocking`] stamps it on
/// every freshly-written document; rows left at a lower value are rebuilt in
/// the background by [`SqliteStore::rebuild_stale_ngrams`].
pub(crate) const NGRAM_INDEX_VERSION: i64 = 1;

/// Documents regenerated per [`SqliteStore::rebuild_stale_ngrams`] call. Kept
/// small so each rebuild transaction holds the single `SQLite` writer lock only
/// briefly against concurrent captures (the daemon worker sleeps between
/// batches); the gram CPU work happens outside the transaction. Index
/// maintenance on the `ngrams` table dominates the per-row cost and grows with
/// the table, so this stays modest to bound the writer-lock hold on very large
/// histories rather than maximizing rebuild throughput.
pub(crate) const NGRAM_REBUILD_BATCH: usize = 50;

impl SqliteStore {
    /// Convenience wrapper that runs a [`SearchQuery`] through the canonical
    /// [`SearchService`] using the `nagori-search` ranker. Kept on the store
    /// so existing callers (CLI, daemon, Tauri, benches) don't have to wire
    /// the service up themselves.
    pub async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        SearchService::new(self, &DefaultRanker).search(query).await
    }

    /// Number of documents whose grams predate [`NGRAM_INDEX_VERSION`] and
    /// therefore still need regenerating. `0` means the ngram index is fully
    /// current. Cheap: an index range scan over
    /// `idx_search_documents_ngram_version_doc_id`.
    pub async fn pending_ngram_rebuild(&self) -> Result<u64> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM search_documents WHERE ngram_index_version < ?1",
                    params![NGRAM_INDEX_VERSION],
                    |row| row.get(0),
                )
                .map_err(storage_err)?;
            Ok(u64::try_from(count).unwrap_or(0))
        })
        .await
    }

    /// Regenerate grams for up to [`NGRAM_REBUILD_BATCH`] documents whose
    /// `ngram_index_version` is stale, returning how many rows were fetched
    /// (`0` once the index is fully current). Call in a loop until it returns
    /// `0`.
    ///
    /// Only the `ngrams` table and the per-row version marker change — the
    /// stored `normalized_text`, FTS index, previews (including the redacted
    /// previews `StoreFull` keeps over a raw body), and semantic vectors are
    /// left untouched, since the grams are regenerated from the already-stored
    /// `normalized_text`. The gram CPU work runs outside the write transaction;
    /// inside it, each row's version is re-checked under the writer lock and
    /// skipped if a concurrent capture already refreshed it (whose grams are
    /// then authoritative), so the worker never clobbers a newer write.
    pub async fn rebuild_stale_ngrams(&self) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            // Read the next stale batch (autocommit) so the CPU-heavy gram
            // generation below doesn't hold the writer lock.
            let batch: Vec<(String, String)> = {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT entry_id, normalized_text
                         FROM search_documents
                         WHERE ngram_index_version < ?1
                         ORDER BY doc_id
                         LIMIT ?2",
                    )
                    .map_err(storage_err)?;
                let limit = i64::try_from(NGRAM_REBUILD_BATCH).unwrap_or(i64::MAX);
                let rows = stmt
                    .query_map(params![NGRAM_INDEX_VERSION, limit], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(storage_err)?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row.map_err(storage_err)?);
                }
                out
            };
            if batch.is_empty() {
                return Ok(0);
            }
            let fetched = batch.len();
            // Generate grams before opening the write transaction.
            let prepared: Vec<(String, Vec<String>)> = batch
                .into_iter()
                .map(|(entry_id, normalized)| (entry_id, generate_document_ngrams(&normalized)))
                .collect();

            let tx = conn.transaction().map_err(storage_err)?;
            {
                let mut current = tx
                    .prepare_cached(
                        "SELECT ngram_index_version FROM search_documents WHERE entry_id = ?1",
                    )
                    .map_err(storage_err)?;
                let mut delete = tx
                    .prepare_cached("DELETE FROM ngrams WHERE entry_id = ?1")
                    .map_err(storage_err)?;
                let mut insert = tx
                    .prepare_cached(
                        "INSERT OR IGNORE INTO ngrams (gram, entry_id, position) VALUES (?1, ?2, ?3)",
                    )
                    .map_err(storage_err)?;
                let mut stamp = tx
                    .prepare_cached(
                        "UPDATE search_documents SET ngram_index_version = ?2 WHERE entry_id = ?1",
                    )
                    .map_err(storage_err)?;
                for (entry_id, grams) in &prepared {
                    // Under the writer lock this read is authoritative: if a
                    // capture refreshed the row since we fetched it (stamping
                    // the current version), its grams are already correct, so
                    // skip rather than overwrite with grams from stale text.
                    let version: i64 = current
                        .query_row(params![entry_id], |row| row.get(0))
                        .optional()
                        .map_err(storage_err)?
                        .unwrap_or(NGRAM_INDEX_VERSION);
                    if version >= NGRAM_INDEX_VERSION {
                        continue;
                    }
                    delete
                        .execute(params![entry_id])
                        .map_err(storage_err)?;
                    for (position, gram) in grams.iter().enumerate() {
                        insert
                            .execute(params![gram, entry_id, position as i64])
                            .map_err(storage_err)?;
                    }
                    stamp
                        .execute(params![entry_id, NGRAM_INDEX_VERSION])
                        .map_err(storage_err)?;
                }
            }
            tx.commit().map_err(storage_err)?;
            Ok(fetched)
        })
        .await
    }
}

#[async_trait]
impl SearchRepository for SqliteStore {
    async fn upsert_document(&self, doc: SearchDocument) -> Result<()> {
        // Detect ngram truncation *before* moving `doc` into the blocking
        // closure. The blocking section runs `generate_document_ngrams` and silently
        // discards everything past `MAX_NGRAM_INPUT_CHARS`; if we did not
        // record a breadcrumb here, a user whose paste is longer than the
        // cap would notice "fuzzy search misses the tail of my entry" with
        // no signal in the DB explaining why. Audit-event writes can't run
        // inside the same transaction (we need a fresh connection), so we
        // do them after the upsert commits and tolerate failures so a
        // transient audit-write error never wedges indexing.
        let truncated = ngram_input_was_truncated(&doc.normalized_text);
        let entry_id = doc.entry_id;
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            upsert_document_blocking(&tx, &doc)?;
            tx.commit().map_err(storage_err)?;
            Ok(())
        })
        .await?;
        if truncated {
            let detail = format!("cap_chars={MAX_NGRAM_INPUT_CHARS}");
            if let Err(err) = nagori_core::AuditLog::record(
                self,
                "ngram_truncated",
                Some(entry_id),
                Some(&detail),
            )
            .await
            {
                tracing::warn!(error = %err, "audit_record_failed");
            }
        }
        Ok(())
    }

    async fn delete_document(&self, entry_id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            delete_search_rows(&tx, &entry_id.to_string())?;
            tx.commit().map_err(storage_err)?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl SearchCandidateProvider for SqliteStore {
    async fn recent_entries(
        &self,
        filters: &SearchFilters,
        order: RecentOrder,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchCandidate>> {
        let filter = build_filter_fragment(filters)?;
        let limit_i64 = clamp_limit(limit);
        self.run_search_blocking(cancel, move |conn| {
            // Same shape as `fetch_recent_entries` (which `list_recent` keeps
            // for full-entry reads) but projected through `CANDIDATE_COLUMNS`
            // so the per-keystroke empty-query path never carries bodies.
            let order_sql = recent_order_sql(order);
            let sql = format!(
                "SELECT {CANDIDATE_COLUMNS}
                 FROM entries e
                 LEFT JOIN search_documents d ON d.entry_id = e.id
                 WHERE e.deleted_at IS NULL
                   AND e.sensitivity != 'blocked'
                   {extra}
                 {order_sql}
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> =
                filter.params.iter().map(|p| &**p as &dyn ToSql).collect();
            bound.push(&limit_i64);
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), row_to_candidate)
                .map_err(storage_err)?;
            let mut candidates = Vec::new();
            for row in rows {
                candidates.push(row.map_err(storage_err)?);
            }
            Ok(candidates)
        })
        .await
    }

    async fn substring_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
        bounded: bool,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchCandidate>> {
        let filter = build_filter_fragment(filters)?;
        let like = format!("%{}%", escape_like(normalized));
        let limit_i64 = clamp_limit(limit);
        let scan_window = SUBSTRING_SCAN_WINDOW;
        self.run_search_blocking(cancel, move |conn| {
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
                     SELECT DISTINCT {CANDIDATE_COLUMNS}
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
                    "SELECT {CANDIDATE_COLUMNS}
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
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> = Vec::new();
            if bounded {
                bound.push(&scan_window);
            }
            bound.push(&like);
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            let mut candidates = Vec::new();
            for row in stmt
                .query_map(rusqlite::params_from_iter(bound), row_to_candidate)
                .map_err(storage_err)?
            {
                candidates.push(row.map_err(storage_err)?);
            }
            Ok(candidates)
        })
        .await
    }

    async fn fulltext_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<FtsCandidate>> {
        let fts = fts_query(normalized);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let filter = build_filter_fragment(filters)?;
        let limit_i64 = clamp_limit(limit);
        self.run_search_blocking(cancel, move |conn| {
            // `search_fts` is an external-content FTS5 over
            // `search_documents`, so it has no `entry_id` column — join via
            // `search_fts.rowid = search_documents.doc_id` (the explicit
            // INTEGER PRIMARY KEY that aliases the source rowid).
            let sql = format!(
                "SELECT {CANDIDATE_COLUMNS},
                        bm25(search_fts) AS fts_score
                 FROM search_fts
                 JOIN search_documents d ON d.doc_id = search_fts.rowid
                 JOIN entries e ON e.id = d.entry_id
                 WHERE search_fts MATCH ?
                   AND e.deleted_at IS NULL
                   AND e.sensitivity != 'blocked'
                   {extra}
                 ORDER BY fts_score
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> = vec![&fts];
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    let score: f64 = row.get("fts_score").unwrap_or(0.0);
                    let candidate = row_to_candidate(row)?;
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(FtsCandidate {
                        candidate,
                        fts_score: score as f32,
                    })
                })
                .map_err(storage_err)?;
            let mut hits = Vec::new();
            for row in rows {
                hits.push(row.map_err(storage_err)?);
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
        mode: NgramQueryMode,
        cancel: &CancellationToken,
    ) -> Result<Vec<NgramCandidate>> {
        let mut query_grams = generate_query_ngrams(normalized);
        if query_grams.is_empty() {
            return Ok(Vec::new());
        }
        match mode {
            // Hybrid (Auto): keep only grams that carry a CJK character. ASCII
            // word recall is already served by FTS + the bounded substring
            // scan, and common ASCII bigrams own huge posting lists whose
            // `gram IN (...)` union explodes on large histories (the 100k
            // fan-out blowup). A pure-ASCII query leaves no grams here and
            // short-circuits to empty; mixed CJK+ASCII queries keep just their
            // CJK / boundary grams, so the costly ASCII postings never load.
            NgramQueryMode::CjkOnly => {
                query_grams.retain(|gram| has_cjk(gram));
                if query_grams.is_empty() {
                    return Ok(Vec::new());
                }
            }
            // Explicit Fuzzy: use the full gram set so ASCII typos still match
            // via overlap, but skip long ASCII queries (LIKE/FTS already cover
            // them) to keep the fan-out bounded.
            NgramQueryMode::Full => {
                if !(has_cjk(normalized) || normalized.chars().count() <= 8) {
                    return Ok(Vec::new());
                }
            }
        }
        let filter = build_filter_fragment(filters)?;
        let limit_i64 = clamp_limit(limit);
        self.run_search_blocking(cancel, move |conn| {
            let placeholders = std::iter::repeat_n("?", query_grams.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT {CANDIDATE_COLUMNS},
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
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> = query_grams.iter().map(|g| g as &dyn ToSql).collect();
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit_i64);
            #[allow(clippy::cast_precision_loss)]
            let total = query_grams.len() as f32;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    let hits: i64 = row.get("hits").unwrap_or(0);
                    let candidate = row_to_candidate(row)?;
                    Ok((candidate, hits))
                })
                .map_err(storage_err)?;
            let mut out = Vec::new();
            for row in rows {
                let (candidate, hits) = row.map_err(storage_err)?;
                #[allow(clippy::cast_precision_loss)]
                let overlap = (hits as f32 / total).clamp(0.0, 1.0);
                out.push(NgramCandidate {
                    candidate,
                    ngram_overlap: overlap,
                });
            }
            Ok(out)
        })
        .await
    }
}

pub(super) fn upsert_document_blocking(
    tx: &rusqlite::Transaction<'_>,
    doc: &SearchDocument,
) -> Result<()> {
    let entry_id = doc.entry_id.to_string();
    // `search_fts` is an external-content FTS5 over `search_documents`;
    // the ai/ad/au triggers keep it in sync, so we only upsert the
    // content row here.
    // Stamp the current generator revision in the same statement that writes
    // the grams below, so a freshly-captured document is never seen as stale by
    // the background rebuild worker (which fetches rows with
    // `ngram_index_version < NGRAM_INDEX_VERSION`).
    tx.execute(
        "INSERT INTO search_documents
            (entry_id, title, preview, normalized_text, language, ngram_index_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(entry_id) DO UPDATE SET
            title = excluded.title,
            preview = excluded.preview,
            normalized_text = excluded.normalized_text,
            language = excluded.language,
            ngram_index_version = excluded.ngram_index_version",
        params![
            entry_id,
            doc.title,
            doc.preview,
            doc.normalized_text,
            doc.language,
            NGRAM_INDEX_VERSION,
        ],
    )
    .map_err(storage_err)?;
    tx.execute("DELETE FROM ngrams WHERE entry_id = ?1", params![entry_id])
        .map_err(storage_err)?;
    let mut stmt = tx
        .prepare("INSERT OR IGNORE INTO ngrams (gram, entry_id, position) VALUES (?1, ?2, ?3)")
        .map_err(storage_err)?;
    for (position, gram) in generate_document_ngrams(&doc.normalized_text)
        .iter()
        .enumerate()
    {
        stmt.execute(params![gram, entry_id, position as i64])
            .map_err(storage_err)?;
    }
    Ok(())
}

pub(super) fn delete_search_rows(tx: &rusqlite::Transaction<'_>, entry_id: &str) -> Result<()> {
    // `search_fts` is an external-content FTS5 over `search_documents`;
    // its ad trigger fires on the search_documents delete below.
    tx.execute(
        "DELETE FROM search_documents WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(storage_err)?;
    tx.execute("DELETE FROM ngrams WHERE entry_id = ?1", params![entry_id])
        .map_err(storage_err)?;
    Ok(())
}

const fn recent_order_sql(order: RecentOrder) -> &'static str {
    match order {
        RecentOrder::ByRecency => "ORDER BY e.created_at DESC",
        RecentOrder::ByUseCount => {
            "ORDER BY e.use_count DESC, COALESCE(e.last_used_at, e.created_at) DESC, e.created_at DESC"
        }
        RecentOrder::PinnedFirstThenRecency => "ORDER BY e.pinned DESC, e.created_at DESC",
    }
}

pub(super) fn fetch_recent_entries(
    conn: &Connection,
    filter: &FilterFragment,
    order: RecentOrder,
    limit: i64,
) -> Result<Vec<ClipboardEntry>> {
    let order_sql = recent_order_sql(order);
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
    let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
    let mut bound: Vec<&dyn ToSql> = filter.params.iter().map(|p| &**p as &dyn ToSql).collect();
    bound.push(&limit);
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound), row_to_entry)
        .map_err(storage_err)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row.map_err(storage_err)?);
    }
    Ok(entries)
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Hard ceiling for any candidate-provider `LIMIT` regardless of caller.
///
/// The `SearchService` already clamps its result limit to 200 and
/// over-samples by 8x, so a legitimate candidate request tops out at
/// 1600. Going an order of magnitude above that absorbs any future
/// growth while still bounding direct callers (FTS, tests, future
/// extensions) that bypass the service. Without this cap a pathological
/// `usize` would either wrap to a negative `LIMIT` (which `SQLite`
/// treats as unbounded) or, if non-negative, force a multi-million-row
/// scan and Vec allocation per keystroke.
const MAX_CANDIDATE_LIMIT: i64 = 10_000;

/// Recent-entry window the substring (LIKE) candidate scanner is restricted
/// to. The substring path can't hit a secondary index for `%term%`, so we
/// trade unbounded recall on very old rows for predictable per-keystroke
/// latency: FTS and ngram still see the entire corpus, this branch only
/// backstops them on the recent window where exact substring matches are
/// most useful.
const SUBSTRING_SCAN_WINDOW: i64 = 5_000;

/// Clamp a `usize` candidate limit down to a safe `i64` for `SQLite`'s
/// `LIMIT` clause. See [`MAX_CANDIDATE_LIMIT`] for the upper bound
/// rationale.
fn clamp_limit(limit: usize) -> i64 {
    i64::try_from(limit)
        .unwrap_or(MAX_CANDIDATE_LIMIT)
        .clamp(0, MAX_CANDIDATE_LIMIT)
}

#[derive(Default)]
pub(super) struct FilterFragment {
    pub(super) sql: String,
    // `Send + Sync` is required because `FilterFragment` is built outside
    // `run_blocking` and then moved into the blocking closure where the
    // actual SQL is executed. Without these bounds the closure can't cross
    // tokio's thread boundary.
    pub(super) params: Vec<Box<dyn ToSql + Send + Sync>>,
}

pub(super) fn build_filter_fragment(filters: &SearchFilters) -> Result<FilterFragment> {
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
        // Bind the RFC 3339 rendering, not an `unwrap_or_default()` empty
        // string: `e.created_at >= ''` is always true, so a format failure
        // would silently disable the lower bound instead of surfacing. The
        // format is effectively infallible, but `format_time` returns the
        // failure as a storage error so the filter never degrades to a no-op.
        fragment.sql.push_str(" AND e.created_at >= ?");
        fragment.params.push(Box::new(format_time(after)?));
    }
    if let Some(before) = filters.created_before {
        fragment.sql.push_str(" AND e.created_at <= ?");
        fragment.params.push(Box::new(format_time(before)?));
    }
    Ok(fragment)
}
