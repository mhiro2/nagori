use std::fmt::Write as _;

use async_trait::async_trait;
use nagori_core::{
    ClipboardEntry, EntryId, FtsCandidate, NgramCandidate, RecentOrder, Result,
    SearchCandidateProvider, SearchDocument, SearchFilters, SearchQuery, SearchRepository,
    SearchResult, SearchService,
};
use nagori_search::{
    DefaultRanker, MAX_NGRAM_INPUT_CHARS, generate_ngrams, has_cjk, ngram_input_was_truncated,
};
use rusqlite::{Connection, ToSql, params};
use time::format_description::well_known::Rfc3339;

use super::SqliteStore;
use super::convert::{fts_query, kind_to_str, row_to_entry, storage_err};

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
impl SearchRepository for SqliteStore {
    async fn upsert_document(&self, doc: SearchDocument) -> Result<()> {
        // Detect ngram truncation *before* moving `doc` into the blocking
        // closure. The blocking section runs `generate_ngrams` and silently
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
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            upsert_document_blocking(&tx, &doc)?;
            tx.commit().map_err(|err| storage_err(&err))?;
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
            let tx = conn.transaction().map_err(|err| storage_err(&err))?;
            delete_search_rows(&tx, &entry_id.to_string())?;
            tx.commit().map_err(|err| storage_err(&err))?;
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
    ) -> Result<Vec<ClipboardEntry>> {
        let filter = build_filter_fragment(filters);
        let limit_i64 = clamp_limit(limit);
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
        let limit_i64 = clamp_limit(limit);
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
            let mut stmt = conn.prepare_cached(&sql).map_err(|err| storage_err(&err))?;
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
        let limit_i64 = clamp_limit(limit);
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // `search_fts` is an external-content FTS5 over
            // `search_documents`, so it has no `entry_id` column — join via
            // `search_fts.rowid = search_documents.doc_id` (the explicit
            // INTEGER PRIMARY KEY that aliases the source rowid).
            let sql = format!(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language,
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
            let mut stmt = conn.prepare_cached(&sql).map_err(|err| storage_err(&err))?;
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
        let limit_i64 = clamp_limit(limit);
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
            let mut stmt = conn.prepare_cached(&sql).map_err(|err| storage_err(&err))?;
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

pub(super) fn upsert_document_blocking(
    tx: &rusqlite::Transaction<'_>,
    doc: &SearchDocument,
) -> Result<()> {
    let entry_id = doc.entry_id.to_string();
    // `search_fts` is an external-content FTS5 over `search_documents`;
    // the ai/ad/au triggers keep it in sync, so we only upsert the
    // content row here.
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

pub(super) fn delete_search_rows(tx: &rusqlite::Transaction<'_>, entry_id: &str) -> Result<()> {
    // `search_fts` is an external-content FTS5 over `search_documents`;
    // its ad trigger fires on the search_documents delete below.
    tx.execute(
        "DELETE FROM search_documents WHERE entry_id = ?1",
        params![entry_id],
    )
    .map_err(|err| storage_err(&err))?;
    tx.execute("DELETE FROM ngrams WHERE entry_id = ?1", params![entry_id])
        .map_err(|err| storage_err(&err))?;
    Ok(())
}

pub(super) fn prune_deleted_search_rows(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // `search_fts` is an external-content FTS5 over `search_documents`;
    // the ad trigger fires on each search_documents row that this DELETE
    // removes, so we don't prune `search_fts` directly.
    tx.execute(
        "DELETE FROM search_documents
         WHERE entry_id IN (SELECT id FROM entries WHERE deleted_at IS NOT NULL)",
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

pub(super) fn fetch_recent_entries(
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
    let mut stmt = conn.prepare_cached(&sql).map_err(|err| storage_err(&err))?;
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

pub(super) fn build_filter_fragment(filters: &SearchFilters) -> FilterFragment {
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
