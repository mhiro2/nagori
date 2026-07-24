//! On-device semantic index backed by `sqlite-vec`.
//!
//! Vectors are stored as raw little-endian float32 BLOBs — the layout
//! `sqlite-vec`'s `vec_distance_cosine` consumes directly — so a query ranks
//! the stored corpus by cosine distance entirely in SQL. The embedder that
//! produces the vectors lives behind `nagori-ai`'s `Embedder` trait; this module
//! only persists, prunes, and queries them, plus tracks the model metadata so a
//! model / revision / dimension change clears the index for a rebuild.
//!
//! Compiled only under the `semantic-index` feature; the daemon enables it so
//! the shipping app has semantic search.
#![allow(unsafe_code)]

use std::os::raw::{c_char, c_int};
use std::sync::Once;

use nagori_core::{
    AppError, EntryId, RankReason, Result, SearchFilters, SearchResult, SemanticIndexMeta,
    Sensitivity, SourceApp,
};
use rusqlite::{OptionalExtension, ToSql, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, parse_sensitivity_strict, row_to_entry, storage_err};
use super::search::build_filter_fragment;

/// Live, embeddable-entry counts for the semantic index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SemanticIndexCounts {
    /// Live, embeddable entries that already have a stored vector.
    pub indexed: u64,
    /// Total live, embeddable entries (vectors + still-pending).
    pub total: u64,
}

/// One entry awaiting (re)embedding: the text to embed plus the entry's content
/// hash, stored alongside the vector so an unchanged entry is never re-embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEmbedding {
    pub entry_id: EntryId,
    /// The normalized search text — what would actually be embedded.
    pub text: String,
    /// Every other persisted text projection of the entry: the raw
    /// plain-text body, the rich-text markup, and the text-shaped rows in
    /// `entry_representations`. The indexer re-assesses *all* of them against
    /// the current policy alongside `text`, mirroring what the capture-time
    /// classifier saw: normalization folds case and width (so a
    /// case-sensitive `regex_denylist` rule or built-in detector — `AKIA…` —
    /// that matched the capture would silently miss the normalized form
    /// alone), and a rule can match a markup / alternative payload that never
    /// reached the plain projection.
    pub raw_texts: Vec<String>,
    /// Set when the stored `content_json` failed to deserialize, so the raw
    /// body could not be recovered for re-assessment. The indexer fails
    /// closed on this (refuses to embed) rather than embedding content it
    /// could not re-check.
    pub content_unparseable: bool,
    pub content_hash: String,
    /// Capture-time classification of the source entry. The indexer treats
    /// this as a *floor*, not the verdict: it re-assesses the text against
    /// the current policy before embedding (a rule added after capture must
    /// still apply) and redacts `Private` bodies so private content never
    /// reaches the model verbatim. `Secret` rows are excluded from
    /// `semantic_pending` entirely, so this never reports `Secret` /
    /// `Blocked`.
    pub sensitivity: Sensitivity,
    /// The entry's recorded source application, when captured, so the
    /// indexer's re-assessment can apply `app_denylist` rules added after
    /// the entry was stored.
    pub source: Option<SourceApp>,
}

/// The entry-point signature `rusqlite`'s `sqlite3_auto_extension` expects.
type AutoExtensionFn = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> c_int;

/// Registers `sqlite-vec`'s scalar functions as a `SQLite` auto-extension, once
/// for the whole process, so every connection opened afterwards can call
/// `vec_distance_cosine`.
///
/// Must be called before any `Connection::open` (the store does this at the top
/// of `open` / `open_memory`).
pub(super) fn register_vec_extension() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: guarded by `Once`, so registration happens exactly once before
        // any connection is opened. The transmute reconciles sqlite-vec's
        // `sqlite3_vec_init` (typed against its own `libsqlite3-sys`) with the
        // entry-point type rusqlite's `sqlite3_auto_extension` expects; the two
        // share one C ABI, so the cast is sound.
        unsafe {
            let init = std::mem::transmute::<*const (), AutoExtensionFn>(
                sqlite_vec::sqlite3_vec_init as *const (),
            );
            rusqlite::ffi::sqlite3_auto_extension(Some(init));
        }
    });
}

/// Recover one pending entry's non-normalized text projections — the raw
/// body and rich-text markup from `content_json`, plus the text-shaped rows
/// of `entry_representations` — for the indexer's policy re-assessment.
///
/// Returns `(raw_texts, content_unparseable)`; the flag is set (and surfaced
/// to the indexer so it can fail closed) when `content_json` no longer
/// deserializes: a body that cannot be re-checked against the current policy
/// must not be embedded on the normalized projection alone.
fn recover_raw_texts(
    reps_stmt: &mut rusqlite::CachedStatement<'_>,
    entry_id: &str,
    content_json: &str,
) -> Result<(Vec<String>, bool)> {
    let mut raw_texts = Vec::new();
    let mut content_unparseable = false;
    match serde_json::from_str::<nagori_core::ClipboardContent>(content_json) {
        Ok(content) => {
            if let Some(raw) = content.plain_text() {
                raw_texts.push(raw.to_owned());
            }
            if let nagori_core::ClipboardContent::RichText(rich) = &content
                && let Some(markup) = rich.markup.as_deref()
            {
                raw_texts.push(markup.to_owned());
            }
        }
        Err(_) => content_unparseable = true,
    }
    let reps = reps_stmt
        .query_map(params![entry_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage_err)?;
    for rep in reps {
        let (mime, text) = rep.map_err(storage_err)?;
        // `text/uri-list` rows store a JSON-encoded path list; the
        // capture-time classifier scanned those paths joined with newlines,
        // so decode back to the same shape — a JSON blob would defeat
        // anchored or quote-sensitive rules that matched at capture.
        // `decode_file_paths` cannot fail (a non-JSON legacy row falls back
        // to its newline form).
        if mime.eq_ignore_ascii_case("text/uri-list") {
            raw_texts.push(nagori_core::decode_file_paths(&text).join("\n"));
        } else {
            raw_texts.push(text);
        }
    }
    Ok((raw_texts, content_unparseable))
}

/// Serialises a vector to the little-endian float32 BLOB `sqlite-vec` reads.
fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

impl SqliteStore {
    /// Reads the persisted embedding-model metadata, if any vectors are stored.
    pub async fn semantic_meta(&self) -> Result<Option<SemanticIndexMeta>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let row = conn
                .query_row(
                    "SELECT model_identifier, revision, dimension, max_sequence_length, \
                     languages, index_version, policy_hash
                     FROM semantic_index_meta WHERE id = 1",
                    [],
                    |row| {
                        let languages: String = row.get(4)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            languages,
                            row.get::<_, i64>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(storage_err)?;
            Ok(row.map(
                |(
                    model_identifier,
                    revision,
                    dimension,
                    max_seq,
                    languages,
                    index_version,
                    policy_hash,
                )| {
                    let languages: Vec<String> =
                        serde_json::from_str(&languages).unwrap_or_default();
                    SemanticIndexMeta {
                        model_identifier,
                        revision: u32::try_from(revision).unwrap_or(0),
                        dimension: u32::try_from(dimension).unwrap_or(0),
                        max_sequence_length: u32::try_from(max_seq).unwrap_or(0),
                        languages,
                        index_version: u32::try_from(index_version).unwrap_or(0),
                        policy_hash,
                    }
                },
            ))
        })
        .await
    }

    /// Replaces the persisted embedding-model metadata (singleton row).
    pub async fn semantic_set_meta(&self, meta: SemanticIndexMeta) -> Result<()> {
        let languages = serde_json::to_string(&meta.languages).unwrap_or_else(|_| "[]".to_owned());
        let now = format_time(OffsetDateTime::now_utc())?;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.execute(
                "INSERT INTO semantic_index_meta
                    (id, model_identifier, revision, dimension, max_sequence_length,
                     languages, index_version, policy_hash, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                    model_identifier = excluded.model_identifier,
                    revision = excluded.revision,
                    dimension = excluded.dimension,
                    max_sequence_length = excluded.max_sequence_length,
                    languages = excluded.languages,
                    index_version = excluded.index_version,
                    policy_hash = excluded.policy_hash,
                    updated_at = excluded.updated_at",
                params![
                    meta.model_identifier,
                    i64::from(meta.revision),
                    i64::from(meta.dimension),
                    i64::from(meta.max_sequence_length),
                    languages,
                    i64::from(meta.index_version),
                    meta.policy_hash,
                    now,
                ],
            )
            .map_err(storage_err)?;
            Ok(())
        })
        .await
    }

    /// Drops every stored vector, the policy-exclusion tombstones, and the
    /// metadata row. Used when the live embedder's model — or the privacy
    /// policy the vectors were embedded under — is incompatible with the
    /// persisted one. Exclusions go with the vectors so the rebuild
    /// re-assesses every entry under the *current* policy rather than
    /// trusting refusals recorded under the old one.
    pub async fn semantic_clear(&self) -> Result<()> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            let deleted = tx
                .execute("DELETE FROM entry_embeddings", [])
                .map_err(storage_err)?;
            tx.execute("DELETE FROM semantic_exclusions", [])
                .map_err(storage_err)?;
            tx.execute("DELETE FROM semantic_index_meta", [])
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            // A clear driven by a policy change is a privacy erase, so follow
            // the same WAL-scrub contract as the hard-delete paths:
            // `secure_delete` zeroes the freed pages in the main DB, and the
            // best-effort truncate checkpoint drops the historical WAL frames
            // that still carry the old vectors.
            super::maintenance::checkpoint_truncate_after_purge(&conn, deleted);
            Ok(())
        })
        .await
    }

    /// Records that the indexer refused to embed these entries under the
    /// current policy (their stored text re-assessed as Secret / Blocked),
    /// and drops any vector they still hold, in one transaction.
    ///
    /// Each item is `(entry_id, content_hash)`. The tombstone keeps the
    /// entry out of `semantic_pending` until its content hash changes (a
    /// rewritten body is re-assessed) or the index is cleared for a rebuild
    /// (a policy change re-assesses everything). Deleting the leftover
    /// vector in the same transaction guarantees a refused entry can never
    /// remain ranked on content the current policy forbids.
    pub async fn semantic_exclude_batch(&self, items: Vec<(EntryId, String)>) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let now = format_time(OffsetDateTime::now_utc())?;
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let mut dropped = 0_usize;
            let tx = conn.transaction().map_err(storage_err)?;
            {
                let mut upsert = tx
                    .prepare_cached(
                        "INSERT INTO semantic_exclusions (entry_id, content_hash, created_at)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(entry_id) DO UPDATE SET
                            content_hash = excluded.content_hash,
                            created_at = excluded.created_at",
                    )
                    .map_err(storage_err)?;
                let mut drop_vector = tx
                    .prepare_cached("DELETE FROM entry_embeddings WHERE entry_id = ?1")
                    .map_err(storage_err)?;
                for (entry_id, content_hash) in &items {
                    let id = entry_id.to_string();
                    upsert
                        .execute(params![id, content_hash, now])
                        .map_err(storage_err)?;
                    dropped += drop_vector.execute(params![id]).map_err(storage_err)?;
                }
            }
            tx.commit().map_err(storage_err)?;
            // A dropped vector held content the current policy forbids, so
            // scrub the WAL frames that still carry it, matching the
            // hard-delete contract. No-op when nothing was deleted (the
            // common case: refused entries usually never had a vector).
            super::maintenance::checkpoint_truncate_after_purge(&conn, dropped);
            Ok(())
        })
        .await
    }

    /// Stores (or replaces) the embedding for one entry.
    pub async fn semantic_upsert(
        &self,
        entry_id: EntryId,
        content_hash: String,
        vector: Vec<f32>,
    ) -> Result<()> {
        self.semantic_upsert_batch_guarded(vec![(entry_id, content_hash, vector)], None)
            .await
    }

    /// [`Self::semantic_upsert_batch_guarded`] without the policy guard, for
    /// callers that manage index consistency themselves.
    pub async fn semantic_upsert_batch(
        &self,
        items: Vec<(EntryId, String, Vec<f32>)>,
    ) -> Result<()> {
        self.semantic_upsert_batch_guarded(items, None).await
    }

    /// Stores (or replaces) the embeddings for a batch of entries in one
    /// transaction.
    ///
    /// The indexer validates the batch (id set, dimensions, no duplicates)
    /// before calling, then relies on the single transaction here so a batch is
    /// applied all-or-nothing: a crash mid-write never leaves the index with
    /// some vectors persisted and their siblings dropped.
    ///
    /// `expected_policy_hash`, when given, is re-checked against
    /// `semantic_index_meta.policy_hash` *inside the write transaction*: a
    /// settings write whose policy fingerprint changed erases the metadata row
    /// in its own transaction (see the settings repository), so a batch that
    /// was shaped under the old policy fails this check and aborts with
    /// [`AppError::Conflict`] instead of committing forbidden vectors. This is
    /// the write-side half of the policy/index atomicity contract; the
    /// indexer's watch-based pre-checks only narrow the window, this guard
    /// closes it.
    pub async fn semantic_upsert_batch_guarded(
        &self,
        items: Vec<(EntryId, String, Vec<f32>)>,
        expected_policy_hash: Option<String>,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        if items.iter().any(|(_, _, vector)| vector.is_empty()) {
            return Err(AppError::storage(
                "refusing to store an empty embedding vector".to_owned(),
            ));
        }
        let now = format_time(OffsetDateTime::now_utc())?;
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            if let Some(expected) = &expected_policy_hash {
                let stored: Option<String> = tx
                    .query_row(
                        "SELECT policy_hash FROM semantic_index_meta WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(storage_err)?;
                if stored.as_deref() != Some(expected.as_str()) {
                    return Err(AppError::Conflict(
                        "semantic index policy changed while the batch was in flight".to_owned(),
                    ));
                }
            }
            {
                let mut stmt = tx
                    .prepare_cached(
                        "INSERT INTO entry_embeddings
                            (entry_id, vector, dimension, content_hash, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(entry_id) DO UPDATE SET
                            vector = excluded.vector,
                            dimension = excluded.dimension,
                            content_hash = excluded.content_hash,
                            created_at = excluded.created_at",
                    )
                    .map_err(storage_err)?;
                for (entry_id, content_hash, vector) in &items {
                    let dimension = i64::try_from(vector.len()).unwrap_or(i64::MAX);
                    let blob = vector_to_blob(vector);
                    stmt.execute(params![
                        entry_id.to_string(),
                        blob,
                        dimension,
                        content_hash,
                        now
                    ])
                    .map_err(storage_err)?;
                }
            }
            tx.commit().map_err(storage_err)?;
            Ok(())
        })
        .await
    }

    /// Removes one entry's embedding (no-op if it has none).
    pub async fn semantic_delete(&self, entry_id: EntryId) -> Result<()> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.execute(
                "DELETE FROM entry_embeddings WHERE entry_id = ?1",
                params![entry_id.to_string()],
            )
            .map_err(storage_err)?;
            Ok(())
        })
        .await
    }

    /// Counts live, embeddable entries and how many already have a vector.
    pub async fn semantic_counts(&self) -> Result<SemanticIndexCounts> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let total: i64 = conn
                .query_row(
                    // Mirror `semantic_pending`'s embeddable predicate (Secret is
                    // never indexed, and a policy-excluded entry is refused, not
                    // outstanding) so progress never shows perpetual pending.
                    "SELECT COUNT(*)
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     LEFT JOIN semantic_exclusions ex ON ex.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0
                       AND (ex.entry_id IS NULL OR ex.content_hash != e.content_hash)",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err)?;
            let indexed: i64 = conn
                .query_row(
                    // Only count vectors whose `content_hash` still matches the
                    // entry: a stale vector (entry re-captured under a new hash)
                    // is pending re-embedding, not indexed, so progress reports
                    // it as outstanding rather than done. Mirrors the
                    // `semantic_pending` predicate, including the exclusion
                    // filter, so `indexed` can never exceed `total`.
                    "SELECT COUNT(*)
                     FROM entry_embeddings em
                     JOIN entries e ON e.id = em.entry_id
                     JOIN search_documents d ON d.entry_id = e.id
                     LEFT JOIN semantic_exclusions ex ON ex.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0
                       AND em.content_hash = e.content_hash
                       AND (ex.entry_id IS NULL OR ex.content_hash != e.content_hash)",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err)?;
            Ok(SemanticIndexCounts {
                indexed: u64::try_from(indexed).unwrap_or(0),
                total: u64::try_from(total).unwrap_or(0),
            })
        })
        .await
    }

    /// Fetches up to `limit` live, embeddable entries that have no stored vector
    /// yet, most-recent first, so the background indexer embeds fresh clips
    /// before backfilling history.
    pub async fn semantic_pending(&self, limit: usize) -> Result<Vec<PendingEmbedding>> {
        // Clamp to `MAX_READ_LIMIT` like `semantic_search` and the other read
        // paths: every caller passes a small batch size today, but a stray
        // `usize::MAX` would otherwise materialise an unbounded `Vec` of
        // pending rows inside the blocking pool.
        let limit = i64::try_from(limit.clamp(1, super::MAX_READ_LIMIT)).unwrap_or(200);
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let mut stmt = conn
                .prepare_cached(
                    // `Secret` is excluded outright: even a `StoreFull` secret's
                    // raw body must never be handed to the embedding model, and a
                    // `StoreRedacted` secret's body is already scrubbed on disk
                    // but is cheaper and clearer to never index. `Private` rows
                    // are returned and redacted in the indexer before embedding.
                    //
                    // An entry is pending when it has no vector *or* its stored
                    // vector's `content_hash` no longer matches the entry's
                    // current hash (the document was rewritten under the same
                    // id). The hash check keeps a stale vector from lingering;
                    // capture alone never mutates an existing entry's hash, but
                    // the predicate guarantees the invariant regardless of how a
                    // row's content changed.
                    //
                    // A `semantic_exclusions` tombstone with a matching hash
                    // means the indexer already re-assessed this exact content
                    // under the current policy and refused to embed it — skip
                    // it so the backfill drains instead of re-offering the same
                    // refused rows forever. A stale tombstone (content changed)
                    // re-pends the entry for a fresh assessment.
                    "SELECT e.id, e.content_hash, d.normalized_text, e.sensitivity,
                            e.source_app_name, e.source_bundle_id, e.source_executable_path,
                            e.content_json
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     LEFT JOIN entry_embeddings em ON em.entry_id = e.id
                     LEFT JOIN semantic_exclusions ex ON ex.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0
                       AND (em.entry_id IS NULL OR em.content_hash != e.content_hash)
                       AND (ex.entry_id IS NULL OR ex.content_hash != e.content_hash)
                     ORDER BY e.created_at DESC
                     LIMIT ?1",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![limit], |row| {
                    let sensitivity = parse_sensitivity_strict(&row.get::<_, String>(3)?)?;
                    let name: Option<String> = row.get(4)?;
                    let bundle_id: Option<String> = row.get(5)?;
                    let executable_path: Option<String> = row.get(6)?;
                    let source =
                        (name.is_some() || bundle_id.is_some() || executable_path.is_some())
                            .then_some(SourceApp {
                                bundle_id,
                                name,
                                executable_path,
                            });
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        sensitivity,
                        source,
                        row.get::<_, String>(7)?,
                    ))
                })
                .map_err(storage_err)?;
            let base: Vec<_> = rows
                .collect::<std::result::Result<_, _>>()
                .map_err(storage_err)?;
            drop(stmt);

            // Recover every other persisted text projection so the indexer's
            // policy re-assessment sees what the capture-time classifier saw:
            // the raw body and rich-text markup from `content_json`, plus the
            // text-shaped representation rows (HTML / RTF / plain fallbacks
            // are persisted verbatim for non-Secret entries and can carry
            // content the plain projection does not).
            let mut reps_stmt = conn
                .prepare_cached(
                    "SELECT mime_type, text_content FROM entry_representations
                     WHERE entry_id = ?1 AND text_content IS NOT NULL
                     ORDER BY ordinal",
                )
                .map_err(storage_err)?;
            let mut pending = Vec::new();
            for (id, content_hash, text, sensitivity, source, content_json) in base {
                let Ok(entry_id) = id.parse::<EntryId>() else {
                    continue;
                };
                let (raw_texts, content_unparseable) =
                    recover_raw_texts(&mut reps_stmt, &id, &content_json)?;
                pending.push(PendingEmbedding {
                    entry_id,
                    text,
                    raw_texts,
                    content_unparseable,
                    content_hash,
                    sensitivity,
                    source,
                });
            }
            Ok(pending)
        })
        .await
    }

    /// Ranks the stored vectors against `query` by cosine distance, returning
    /// the closest live, non-blocked entries as [`SearchResult`]s.
    ///
    /// `policy_hash`, when given, pins the ranking to an index built under
    /// that exact privacy policy *inside the query's own snapshot*: the SQL
    /// re-checks `semantic_index_meta.policy_hash` in the same statement that
    /// reads the vectors, so a policy edit racing this query (checked by the
    /// caller moments earlier, purged by the worker moments later) yields no
    /// rows instead of ranking vectors the new policy forbids.
    pub async fn semantic_search(
        &self,
        query: Vec<f32>,
        filters: SearchFilters,
        limit: usize,
        policy_hash: Option<String>,
    ) -> Result<Vec<SearchResult>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let dimension = i64::try_from(query.len()).unwrap_or(i64::MAX);
        let blob = vector_to_blob(&query);
        let limit = i64::try_from(limit.clamp(1, super::MAX_READ_LIMIT)).unwrap_or(200);
        let filter = build_filter_fragment(&filters)?;
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // The `em.dimension = ?` guard keeps `vec_distance_cosine` from
            // seeing a stored vector of a different width mid-rebuild (it errors
            // on a dimension mismatch); incompatible models are cleared up
            // front, so in steady state every row already matches.
            //
            // `em.content_hash = e.content_hash` drops stale vectors: a row
            // whose document changed under the same id is awaiting re-embedding
            // (see `semantic_pending`), so ranking against its old vector would
            // surface a result scored on content the entry no longer holds.
            //
            // The `semantic_exclusions` anti-join is defence in depth: an
            // excluded entry's vector is deleted in the same transaction that
            // records the tombstone, so in steady state the join filters
            // nothing — but it guarantees a refused entry can never rank even
            // if a vector slips back in through some future write path.
            let policy_guard = if policy_hash.is_some() {
                "AND EXISTS (SELECT 1 FROM semantic_index_meta m
                             WHERE m.id = 1 AND m.policy_hash = ?)"
            } else {
                ""
            };
            let sql = format!(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language,
                        vec_distance_cosine(em.vector, ?) AS dist
                 FROM entry_embeddings em
                 JOIN entries e ON e.id = em.entry_id
                 JOIN search_documents d ON d.entry_id = e.id
                 LEFT JOIN semantic_exclusions ex ON ex.entry_id = e.id
                 WHERE e.deleted_at IS NULL
                   AND e.sensitivity NOT IN ('blocked', 'secret')
                   AND em.dimension = ?
                   AND em.content_hash = e.content_hash
                   AND (ex.entry_id IS NULL OR ex.content_hash != e.content_hash)
                   {policy_guard}
                   {extra}
                 ORDER BY dist ASC
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> = vec![&blob, &dimension];
            if let Some(hash) = &policy_hash {
                bound.push(hash);
            }
            bound.extend(filter.params.iter().map(|p| &**p as &dyn ToSql));
            bound.push(&limit);
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    let dist: f64 = row.get("dist").unwrap_or(2.0);
                    let entry = row_to_entry(row)?;
                    // Cosine distance ∈ [0, 2]; similarity = 1 − distance.
                    #[allow(clippy::cast_possible_truncation)]
                    let score = (1.0 - dist) as f32;
                    let source_app_name = entry
                        .metadata
                        .source
                        .as_ref()
                        .and_then(|source| source.name.clone());
                    let language = entry.search.language.clone();
                    let (image_width, image_height) = match &entry.content {
                        nagori_core::ClipboardContent::Image(image) => (image.width, image.height),
                        _ => (None, None),
                    };
                    Ok(SearchResult {
                        entry_id: entry.id,
                        score,
                        rank_reason: vec![RankReason::SemanticMatch],
                        content_kind: entry.content_kind(),
                        created_at: entry.metadata.created_at,
                        pinned: entry.lifecycle.pinned,
                        sensitivity: entry.sensitivity,
                        preview: entry.search.preview,
                        source_app_name,
                        language,
                        image_width,
                        image_height,
                    })
                })
                .map_err(storage_err)?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row.map_err(storage_err)?);
            }
            Ok(results)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::{EntryFactory, EntryRepository, SearchFilters};

    use crate::SqliteStore;

    use super::*;

    fn meta(revision: u32, dimension: u32) -> SemanticIndexMeta {
        SemanticIndexMeta {
            model_identifier: "test-model".to_owned(),
            revision,
            dimension,
            max_sequence_length: 256,
            languages: vec!["en".to_owned()],
            index_version: 1,
            policy_hash: "policy-hash-a".to_owned(),
        }
    }

    async fn insert_text(store: &SqliteStore, text: &str) -> EntryId {
        store.insert(EntryFactory::from_text(text)).await.unwrap()
    }

    async fn insert_with_sensitivity(
        store: &SqliteStore,
        text: &str,
        sensitivity: Sensitivity,
    ) -> EntryId {
        let mut entry = EntryFactory::from_text(text);
        entry.sensitivity = sensitivity;
        store.insert(entry).await.unwrap()
    }

    #[tokio::test]
    async fn meta_round_trips_and_clears() {
        let store = SqliteStore::open_memory().unwrap();
        assert!(store.semantic_meta().await.unwrap().is_none());

        store.semantic_set_meta(meta(1, 4)).await.unwrap();
        assert_eq!(store.semantic_meta().await.unwrap(), Some(meta(1, 4)));

        store.semantic_clear().await.unwrap();
        assert!(store.semantic_meta().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pending_lists_unembedded_entries_and_counts_track_upserts() {
        let store = SqliteStore::open_memory().unwrap();
        let a = insert_text(&store, "alpha document").await;
        let _b = insert_text(&store, "beta document").await;

        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.total, 2);
        assert_eq!(counts.indexed, 0);

        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        // Store the vector under the entry's real content hash so the counts
        // see it as indexed (a mismatching hash keeps the entry pending).
        let hash_a = pending
            .iter()
            .find(|p| p.entry_id == a)
            .unwrap()
            .content_hash
            .clone();

        store
            .semantic_upsert(a, hash_a, vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.indexed, 1);
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].entry_id, a);
    }

    #[tokio::test]
    async fn stale_hash_vector_stays_pending_and_uncounted() {
        // A vector whose `content_hash` no longer matches the entry (the
        // document was rewritten under the same id) must be treated as pending
        // re-embedding, not as indexed — otherwise an outdated vector would
        // linger and the entry would never be re-embedded.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "rewritten document").await;

        // Store under a hash that deliberately differs from the entry's.
        store
            .semantic_upsert(id, "stale-hash".to_owned(), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();

        // Stale vector ⇒ still pending, not counted as indexed.
        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.indexed, 0);
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].entry_id, id);

        // Re-embed under the current hash ⇒ no longer pending, now indexed.
        store
            .semantic_upsert(id, pending[0].content_hash.clone(), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);
        assert!(store.semantic_pending(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn excluded_entry_leaves_pending_and_counts_until_content_changes() {
        let store = SqliteStore::open_memory().unwrap();
        let refused = insert_text(&store, "matches a denylist rule now").await;
        let normal = insert_text(&store, "plain note").await;

        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        let hash = pending
            .iter()
            .find(|p| p.entry_id == refused)
            .unwrap()
            .content_hash
            .clone();

        store
            .semantic_exclude_batch(vec![(refused, hash.clone())])
            .await
            .unwrap();

        // The refused entry no longer counts as embeddable or pending, so the
        // backfill can drain and progress reaches "ready".
        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.total, 1);
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].entry_id, normal);

        // A stale tombstone (the entry's content hash moved on) re-pends the
        // entry for a fresh assessment under the current policy.
        store
            .semantic_exclude_batch(vec![(refused, "old-content-hash".to_owned())])
            .await
            .unwrap();
        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2, "a stale exclusion must not stick");
        assert_eq!(store.semantic_counts().await.unwrap().total, 2);
    }

    #[tokio::test]
    async fn exclude_batch_drops_any_leftover_vector() {
        // Excluding an entry must also remove its stored vector in the same
        // transaction: the exclusion means the current policy forbids ranking
        // this content, so a leftover vector (e.g. one embedded under a stale
        // hash) can never survive it.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "previously embedded").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store
            .semantic_upsert(id, hash.clone(), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);

        store
            .semantic_exclude_batch(vec![(id, hash)])
            .await
            .unwrap();

        assert_eq!(store.semantic_counts().await.unwrap().indexed, 0);
        let hits = store
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert!(hits.is_empty(), "an excluded entry must not stay ranked");
    }

    #[tokio::test]
    async fn clear_wipes_exclusions_for_a_fresh_policy_assessment() {
        // A rebuild (policy or model change) must re-assess every entry under
        // the current policy: refusals recorded under the old policy go with
        // the vectors.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "refused under the old policy").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store.semantic_set_meta(meta(1, 4)).await.unwrap();
        store
            .semantic_exclude_batch(vec![(id, hash)])
            .await
            .unwrap();
        assert!(store.semantic_pending(10).await.unwrap().is_empty());

        store.semantic_clear().await.unwrap();

        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "cleared exclusions must re-pend the entry"
        );
        assert_eq!(pending[0].entry_id, id);
    }

    #[tokio::test]
    async fn pending_recovers_representation_texts_for_reassessment() {
        use nagori_core::{
            RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation,
        };

        // The policy re-assessment must see the same payloads the capture-time
        // classifier saw: HTML / RTF alternatives are persisted verbatim for
        // non-Secret entries and can carry content the plain projection does
        // not, so `semantic_pending` recovers them alongside the raw body.
        let store = SqliteStore::open_memory().unwrap();
        let mut entry = EntryFactory::from_text("plain body");
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/plain".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("plain body".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "text/html".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::InlineText(
                    "<p>ticket ACME-1234 hides here</p>".to_owned(),
                ),
            },
        ];
        store.insert(entry).await.unwrap();

        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].content_unparseable);
        assert!(
            pending[0].raw_texts.iter().any(|t| t == "plain body"),
            "raw body must be recovered: {:?}",
            pending[0].raw_texts
        );
        assert!(
            pending[0].raw_texts.iter().any(|t| t.contains("ACME-1234")),
            "alternative-representation text must be recovered: {:?}",
            pending[0].raw_texts
        );
    }

    #[tokio::test]
    async fn search_policy_hash_pin_gates_results_in_the_same_snapshot() {
        // The query path pins the ranking to an index built under the exact
        // policy it validated moments earlier: the pin is re-checked inside
        // the search SQL itself, so a policy edit racing the query yields no
        // rows instead of vectors the new policy forbids.
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "rankable document").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store.semantic_set_meta(meta(1, 3)).await.unwrap();
        store
            .semantic_upsert(id, hash, vec![1.0, 0.0, 0.0])
            .await
            .unwrap();

        let hits = |policy: Option<&str>| {
            let store = store.clone();
            let policy = policy.map(ToOwned::to_owned);
            async move {
                store
                    .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, policy)
                    .await
                    .unwrap()
                    .len()
            }
        };
        assert_eq!(hits(None).await, 1, "unpinned search ranks normally");
        assert_eq!(
            hits(Some("policy-hash-a")).await,
            1,
            "a matching pin ranks normally"
        );
        assert_eq!(
            hits(Some("some-newer-policy")).await,
            0,
            "a mismatching pin must return nothing"
        );
    }

    #[tokio::test]
    async fn policy_changing_settings_save_erases_the_index_in_the_same_transaction() {
        use nagori_core::SettingsRepository;

        // The settings write is the atomicity anchor: once a save whose
        // embedding policy fingerprint differs commits, no snapshot can see
        // old-policy vectors any more — the erase rides in the same
        // transaction rather than waiting for the background worker.
        let store = SqliteStore::open_memory().unwrap();
        let base = nagori_core::AppSettings::default();
        store.save_settings(base.clone()).await.unwrap();

        let id = insert_text(&store, "ticket ACME-1234 details").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        let mut current_meta = meta(1, 3);
        current_meta.policy_hash = base.semantic_policy_hash();
        store.semantic_set_meta(current_meta.clone()).await.unwrap();
        store
            .semantic_upsert(id, hash, vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);

        // An unrelated settings change must NOT invalidate the index.
        let mut unrelated = base.clone();
        unrelated.paste_delay_ms += 10;
        store.save_settings(unrelated.clone()).await.unwrap();
        assert!(store.semantic_meta().await.unwrap().is_some());
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);

        // A policy change (new denylist rule) erases vectors + meta with the
        // save itself.
        let mut stricter = unrelated;
        stricter.regex_denylist = vec!["ACME-\\d+".to_owned()];
        store.save_settings(stricter).await.unwrap();
        assert!(
            store.semantic_meta().await.unwrap().is_none(),
            "the policy-changing save must erase the index metadata"
        );
        assert_eq!(
            store.semantic_counts().await.unwrap().indexed,
            0,
            "the policy-changing save must erase the stored vectors"
        );
    }

    #[tokio::test]
    async fn policy_changing_checked_save_also_erases_the_index() {
        use nagori_core::SettingsRepository;

        let store = SqliteStore::open_memory().unwrap();
        let base = nagori_core::AppSettings::default();
        store.save_settings(base.clone()).await.unwrap();
        let (_, revision) = store.get_settings_with_revision().await.unwrap();

        let id = insert_text(&store, "some document").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        let mut current_meta = meta(1, 3);
        current_meta.policy_hash = base.semantic_policy_hash();
        store.semantic_set_meta(current_meta).await.unwrap();
        store
            .semantic_upsert(id, hash, vec![1.0, 0.0, 0.0])
            .await
            .unwrap();

        let mut stricter = base;
        stricter.otp_detection = !stricter.otp_detection;
        store
            .save_settings_checked(stricter, revision)
            .await
            .unwrap();
        assert!(store.semantic_meta().await.unwrap().is_none());
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 0);
    }

    #[tokio::test]
    async fn guarded_upsert_aborts_when_the_policy_meta_no_longer_matches() {
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "a document").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();

        // Matching guard: the batch commits.
        store.semantic_set_meta(meta(1, 3)).await.unwrap();
        store
            .semantic_upsert_batch_guarded(
                vec![(id, hash.clone(), vec![1.0, 0.0, 0.0])],
                Some("policy-hash-a".to_owned()),
            )
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);

        // A policy-changing save erased the meta row: the guard must abort
        // the write instead of re-inserting old-policy vectors.
        store.semantic_clear().await.unwrap();
        let err = store
            .semantic_upsert_batch_guarded(
                vec![(id, hash.clone(), vec![1.0, 0.0, 0.0])],
                Some("policy-hash-a".to_owned()),
            )
            .await
            .expect_err("a missing meta row must abort a guarded batch");
        assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 0);

        // Same for a meta row rebuilt under a different policy.
        let mut other = meta(1, 3);
        other.policy_hash = "some-newer-policy".to_owned();
        store.semantic_set_meta(other).await.unwrap();
        let err = store
            .semantic_upsert_batch_guarded(
                vec![(id, hash, vec![1.0, 0.0, 0.0])],
                Some("policy-hash-a".to_owned()),
            )
            .await
            .expect_err("a mismatching policy hash must abort a guarded batch");
        assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 0);
    }

    #[tokio::test]
    async fn pending_decodes_file_path_representations_to_the_capture_shape() {
        use nagori_core::{
            RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation,
        };

        // FilePaths representations are persisted as a JSON array, but the
        // capture-time classifier scanned the newline-joined paths — the
        // re-assessment must see that same shape or anchored / quote-sensitive
        // rules that matched at capture would miss.
        let store = SqliteStore::open_memory().unwrap();
        let mut entry = EntryFactory::from_text("some note");
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/plain".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("some note".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "text/uri-list".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::FilePaths(vec![
                    "/Users/private/secret-plan.txt".to_owned(),
                    "/tmp/other.txt".to_owned(),
                ]),
            },
        ];
        store.insert(entry).await.unwrap();

        let pending = store.semantic_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0]
                .raw_texts
                .contains(&"/Users/private/secret-plan.txt\n/tmp/other.txt".to_owned()),
            "file paths must be re-assessed newline-joined, as at capture: {:?}",
            pending[0].raw_texts
        );
        assert!(
            !pending[0].raw_texts.iter().any(|t| t.starts_with('[')),
            "the JSON-encoded storage form must not be what gets scanned: {:?}",
            pending[0].raw_texts
        );
    }

    #[tokio::test]
    async fn pending_carries_the_source_app_for_reassessment() {
        use nagori_core::SourceApp;

        let store = SqliteStore::open_memory().unwrap();
        let mut entry = EntryFactory::from_text("copied from a vault");
        entry.metadata.source = Some(SourceApp {
            bundle_id: Some("com.example.vault".to_owned()),
            name: Some("Example Vault".to_owned()),
            executable_path: None,
        });
        let with_source = store.insert(entry).await.unwrap();
        let plain = insert_text(&store, "no source recorded").await;

        let pending = store.semantic_pending(10).await.unwrap();
        let sourced = pending.iter().find(|p| p.entry_id == with_source).unwrap();
        assert_eq!(
            sourced.source.as_ref().and_then(|s| s.bundle_id.as_deref()),
            Some("com.example.vault")
        );
        let unsourced = pending.iter().find(|p| p.entry_id == plain).unwrap();
        assert!(unsourced.source.is_none());
    }

    #[tokio::test]
    async fn upsert_batch_persists_all_or_rejects_empty() {
        let store = SqliteStore::open_memory().unwrap();
        let a = insert_text(&store, "first").await;
        let b = insert_text(&store, "second").await;
        let pending = store.semantic_pending(10).await.unwrap();
        let hash = |id: EntryId| {
            pending
                .iter()
                .find(|p| p.entry_id == id)
                .unwrap()
                .content_hash
                .clone()
        };

        store
            .semantic_upsert_batch(vec![
                (a, hash(a), vec![1.0, 0.0]),
                (b, hash(b), vec![0.0, 1.0]),
            ])
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 2);

        // An empty vector anywhere in the batch is refused outright.
        let err = store
            .semantic_upsert_batch(vec![(a, hash(a), Vec::new())])
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn pending_excludes_secret_and_flags_private() {
        let store = SqliteStore::open_memory().unwrap();
        let public = insert_with_sensitivity(&store, "public doc", Sensitivity::Public).await;
        let private = insert_with_sensitivity(&store, "private doc", Sensitivity::Private).await;
        let _secret = insert_with_sensitivity(&store, "secret doc", Sensitivity::Secret).await;

        // Secret is never embeddable, so neither the pending list nor the
        // total count includes it.
        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.total, 2);

        let pending = store.semantic_pending(10).await.unwrap();
        let ids: Vec<_> = pending.iter().map(|p| p.entry_id).collect();
        assert!(ids.contains(&public));
        assert!(ids.contains(&private));
        assert_eq!(pending.len(), 2);

        // The indexer needs the sensitivity to decide whether to redact.
        let private_pending = pending.iter().find(|p| p.entry_id == private).unwrap();
        assert_eq!(private_pending.sensitivity, Sensitivity::Private);
    }

    #[tokio::test]
    async fn search_ranks_by_cosine_distance() {
        let store = SqliteStore::open_memory().unwrap();
        let near = insert_text(&store, "near").await;
        let far = insert_text(&store, "far").await;

        // Store under the entries' real content hashes so the vectors are
        // considered current and searchable.
        let pending = store.semantic_pending(10).await.unwrap();
        let hash = |id: EntryId| {
            pending
                .iter()
                .find(|p| p.entry_id == id)
                .unwrap()
                .content_hash
                .clone()
        };

        // Query is closest to `near`'s vector.
        store
            .semantic_upsert(near, hash(near), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .semantic_upsert(far, hash(far), vec![0.0, 1.0, 0.0])
            .await
            .unwrap();

        let results = store
            .semantic_search(vec![0.9, 0.1, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry_id, near);
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].rank_reason, vec![RankReason::SemanticMatch]);
    }

    #[tokio::test]
    async fn search_excludes_stale_hash_vectors() {
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "rankable document").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();

        // A fresh vector (hash matches the entry) ranks normally.
        store
            .semantic_upsert(id, hash, vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        let hits = store
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        // Overwriting with a stale hash (entry awaiting re-embedding) drops the
        // vector from search results rather than ranking on outdated content.
        store
            .semantic_upsert(id, "stale".to_owned(), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        let hits = store
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_ignores_mismatched_dimensions() {
        let store = SqliteStore::open_memory().unwrap();
        let three = insert_text(&store, "three-dim").await;
        store
            .semantic_upsert(three, "h".to_owned(), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();

        // A 4-dim query must not match the stored 3-dim vector.
        let results = store
            .semantic_search(vec![1.0, 0.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delete_removes_vector() {
        let store = SqliteStore::open_memory().unwrap();
        let id = insert_text(&store, "to delete").await;
        let hash = store.semantic_pending(10).await.unwrap()[0]
            .content_hash
            .clone();
        store
            .semantic_upsert(id, hash, vec![1.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 1);
        store.semantic_delete(id).await.unwrap();
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 0);
    }

    #[tokio::test]
    async fn soft_deleted_entries_drop_out_of_search_pending_and_counts() {
        // `mark_deleted` only stamps `deleted_at`; the vector row survives until
        // the maintenance purge. Every semantic query joins on
        // `e.deleted_at IS NULL`, so a soft-deleted entry must vanish from
        // search, the pending backlog, and the counts immediately — long before
        // its vector is physically removed.
        let store = SqliteStore::open_memory().unwrap();
        let keep = insert_text(&store, "keep this vector").await;
        let drop = insert_text(&store, "drop this vector").await;

        let pending = store.semantic_pending(10).await.unwrap();
        let hash = |id: EntryId| {
            pending
                .iter()
                .find(|p| p.entry_id == id)
                .unwrap()
                .content_hash
                .clone()
        };
        store
            .semantic_upsert(keep, hash(keep), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .semantic_upsert(drop, hash(drop), vec![1.0, 0.0, 0.0])
            .await
            .unwrap();

        // Both are searchable and counted while live.
        let hits = store
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(store.semantic_counts().await.unwrap().total, 2);
        assert_eq!(store.semantic_counts().await.unwrap().indexed, 2);

        store.mark_deleted(drop).await.unwrap();

        // The soft-deleted entry is gone from every read path even though its
        // vector row has not been purged yet.
        let hits = store
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, keep);

        let counts = store.semantic_counts().await.unwrap();
        assert_eq!(counts.total, 1, "soft-deleted entry leaves the total");
        assert_eq!(
            counts.indexed, 1,
            "soft-deleted entry leaves the indexed count"
        );

        assert!(
            store
                .semantic_pending(10)
                .await
                .unwrap()
                .iter()
                .all(|p| p.entry_id != drop),
            "a soft-deleted entry must not reappear in the embedding backlog"
        );
    }

    #[tokio::test]
    async fn soft_deleted_unembedded_entry_leaves_the_pending_backlog() {
        // The pending list must also drop soft-deleted entries that were never
        // embedded — otherwise the indexer would keep computing embeddings for
        // rows that are on their way out.
        let store = SqliteStore::open_memory().unwrap();
        let live = insert_text(&store, "still pending").await;
        let gone = insert_text(&store, "deleted before embedding").await;

        store.mark_deleted(gone).await.unwrap();

        let pending = store.semantic_pending(10).await.unwrap();
        let ids: Vec<_> = pending.iter().map(|p| p.entry_id).collect();
        assert_eq!(ids, vec![live]);
        assert_eq!(store.semantic_counts().await.unwrap().total, 1);
    }
}
