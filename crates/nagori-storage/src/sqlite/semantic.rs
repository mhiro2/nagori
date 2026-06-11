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
    Sensitivity,
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
    pub text: String,
    pub content_hash: String,
    /// Classification of the source entry. The indexer redacts `Private`
    /// bodies before embedding so private content never reaches the model
    /// verbatim; `Secret` rows are excluded from `semantic_pending` entirely,
    /// so this never reports `Secret` / `Blocked`.
    pub sensitivity: Sensitivity,
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
                     languages, index_version
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
                        ))
                    },
                )
                .optional()
                .map_err(storage_err)?;
            Ok(row.map(
                |(model_identifier, revision, dimension, max_seq, languages, index_version)| {
                    let languages: Vec<String> =
                        serde_json::from_str(&languages).unwrap_or_default();
                    SemanticIndexMeta {
                        model_identifier,
                        revision: u32::try_from(revision).unwrap_or(0),
                        dimension: u32::try_from(dimension).unwrap_or(0),
                        max_sequence_length: u32::try_from(max_seq).unwrap_or(0),
                        languages,
                        index_version: u32::try_from(index_version).unwrap_or(0),
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
                     languages, index_version, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    model_identifier = excluded.model_identifier,
                    revision = excluded.revision,
                    dimension = excluded.dimension,
                    max_sequence_length = excluded.max_sequence_length,
                    languages = excluded.languages,
                    index_version = excluded.index_version,
                    updated_at = excluded.updated_at",
                params![
                    meta.model_identifier,
                    i64::from(meta.revision),
                    i64::from(meta.dimension),
                    i64::from(meta.max_sequence_length),
                    languages,
                    i64::from(meta.index_version),
                    now,
                ],
            )
            .map_err(storage_err)?;
            Ok(())
        })
        .await
    }

    /// Drops every stored vector and the metadata row. Used when the live
    /// embedder's model is incompatible with the persisted one.
    pub async fn semantic_clear(&self) -> Result<()> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn.transaction().map_err(storage_err)?;
            tx.execute("DELETE FROM entry_embeddings", [])
                .map_err(storage_err)?;
            tx.execute("DELETE FROM semantic_index_meta", [])
                .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
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
        self.semantic_upsert_batch(vec![(entry_id, content_hash, vector)])
            .await
    }

    /// Stores (or replaces) the embeddings for a batch of entries in one
    /// transaction.
    ///
    /// The indexer validates the batch (id set, dimensions, no duplicates)
    /// before calling, then relies on the single transaction here so a batch is
    /// applied all-or-nothing: a crash mid-write never leaves the index with
    /// some vectors persisted and their siblings dropped.
    pub async fn semantic_upsert_batch(
        &self,
        items: Vec<(EntryId, String, Vec<f32>)>,
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
                    // never indexed) so progress never shows perpetual pending.
                    "SELECT COUNT(*)
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0",
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
                    // `semantic_pending` predicate.
                    "SELECT COUNT(*)
                     FROM entry_embeddings em
                     JOIN entries e ON e.id = em.entry_id
                     JOIN search_documents d ON d.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0
                       AND em.content_hash = e.content_hash",
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
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
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
                    "SELECT e.id, e.content_hash, d.normalized_text, e.sensitivity
                     FROM entries e
                     JOIN search_documents d ON d.entry_id = e.id
                     LEFT JOIN entry_embeddings em ON em.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.sensitivity NOT IN ('blocked', 'secret')
                       AND length(d.normalized_text) > 0
                       AND (em.entry_id IS NULL OR em.content_hash != e.content_hash)
                     ORDER BY e.created_at DESC
                     LIMIT ?1",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![limit], |row| {
                    let sensitivity = parse_sensitivity_strict(&row.get::<_, String>(3)?)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        sensitivity,
                    ))
                })
                .map_err(storage_err)?;
            let mut pending = Vec::new();
            for row in rows {
                let (id, content_hash, text, sensitivity) = row.map_err(storage_err)?;
                let Ok(entry_id) = id.parse::<EntryId>() else {
                    continue;
                };
                pending.push(PendingEmbedding {
                    entry_id,
                    text,
                    content_hash,
                    sensitivity,
                });
            }
            Ok(pending)
        })
        .await
    }

    /// Ranks the stored vectors against `query` by cosine distance, returning
    /// the closest live, non-blocked entries as [`SearchResult`]s.
    pub async fn semantic_search(
        &self,
        query: Vec<f32>,
        filters: SearchFilters,
        limit: usize,
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
            let sql = format!(
                "SELECT e.*, d.title, d.preview, d.normalized_text, d.language,
                        vec_distance_cosine(em.vector, ?) AS dist
                 FROM entry_embeddings em
                 JOIN entries e ON e.id = em.entry_id
                 JOIN search_documents d ON d.entry_id = e.id
                 WHERE e.deleted_at IS NULL
                   AND e.sensitivity NOT IN ('blocked', 'secret')
                   AND em.dimension = ?
                   AND em.content_hash = e.content_hash
                   {extra}
                 ORDER BY dist ASC
                 LIMIT ?",
                extra = filter.sql,
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(storage_err)?;
            let mut bound: Vec<&dyn ToSql> = vec![&blob, &dimension];
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
            .semantic_search(vec![0.9, 0.1, 0.0], SearchFilters::default(), 10)
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
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10)
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
            .semantic_search(vec![1.0, 0.0, 0.0], SearchFilters::default(), 10)
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
            .semantic_search(vec![1.0, 0.0, 0.0, 0.0], SearchFilters::default(), 10)
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
}
