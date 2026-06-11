use std::collections::HashMap;
use std::str::FromStr;

use async_trait::async_trait;
use nagori_core::{
    AppError, AuditLog, ClipboardContent, ClipboardEntry, EntryId, EntryMetadata, EntryRepository,
    RecentOrder, RepresentationDataRef, RepresentationRole, RepresentationSummary, Result,
    StoredClipboardRepresentation,
};
use nagori_search::{MAX_NGRAM_INPUT_CHARS, ngram_input_was_truncated};
use rusqlite::{OptionalExtension, ToSql, TransactionBehavior, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{
    bool_int, format_opt_time, format_time, json_err, kind_to_str, row_to_entry,
    sensitivity_to_str, storage_err,
};
use super::search::{
    FilterFragment, delete_search_rows, fetch_recent_entries, upsert_document_blocking,
};
use super::{MAX_READ_LIMIT, clamp_read_limit};

impl SqliteStore {
    pub async fn set_pinned(&self, id: EntryId, pinned: bool) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let changed = {
                let conn = store.conn()?;
                conn.execute(
                    "UPDATE entries SET pinned = ?1, updated_at = ?2 WHERE id = ?3 AND deleted_at IS NULL",
                    params![bool_int(pinned), now, id.to_string()],
                )
                .map_err(storage_err)?
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
                .map_err(storage_err)?
            };
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await
    }

    /// Returns the primary representation's payload bytes and recorded MIME
    /// for an entry, or `None` if no representation row carries inline bytes
    /// (e.g. text-shaped entries) or the entry has been soft-deleted.
    ///
    /// Image bytes live in `entry_representations.payload_blob`; the preview
    /// scheme reads them here. Text-shaped entries deliberately return `None`
    /// because they have no byte payload distinct from the inline text.
    pub async fn get_payload(&self, id: EntryId) -> Result<Option<(Vec<u8>, String)>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT r.payload_blob, r.mime_type
                 FROM entry_representations r
                 JOIN entries e ON e.id = r.entry_id
                 WHERE e.id = ?1
                   AND e.deleted_at IS NULL
                   AND r.role = 'primary'
                   AND r.payload_blob IS NOT NULL
                 ORDER BY r.ordinal
                 LIMIT 1",
                params![id.to_string()],
                |row| {
                    let blob: Option<Vec<u8>> = row.get(0)?;
                    let mime: Option<String> = row.get(1)?;
                    Ok(blob.zip(mime))
                },
            )
            .optional()
            .map_err(storage_err)
            .map(Option::flatten)
        })
        .await
    }

    /// Returns the bytes and recorded MIME of the first non-primary image
    /// representation for an entry, or `None` when the entry carries none.
    ///
    /// A file copy frequently rides alongside an image render of the same
    /// content (e.g. a presentation copied from Finder also places an
    /// `image/png` on the clipboard). That image is kept as a non-primary
    /// representation, so [`Self::get_payload`] — which only reads the
    /// primary row — never finds it. The thumbnail generator falls back to
    /// this lookup so such entries can still show a preview image.
    ///
    /// Candidates are restricted to the MIME allow-list the preview scheme
    /// will actually serve, and ordered by `(ordinal, role)` so the first
    /// image the clipboard provider attached wins.
    pub async fn get_alternate_image_payload(
        &self,
        id: EntryId,
    ) -> Result<Option<(Vec<u8>, String)>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // Bind the allow-list rather than inlining the MIME strings so
            // the set stays the single source of truth in `nagori_core` and
            // can't drift from the signature detector / scheme handler.
            let placeholders = (0..nagori_core::SUPPORTED_IMAGE_MIMES.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT r.payload_blob, r.mime_type
                 FROM entry_representations r
                 JOIN entries e ON e.id = r.entry_id
                 WHERE e.id = ?1
                   AND e.deleted_at IS NULL
                   AND r.role <> 'primary'
                   AND r.payload_blob IS NOT NULL
                   AND LOWER(r.mime_type) IN ({placeholders})
                 ORDER BY r.ordinal, r.role
                 LIMIT 1"
            );
            let id_str = id.to_string();
            let mut sql_params: Vec<&dyn ToSql> =
                Vec::with_capacity(nagori_core::SUPPORTED_IMAGE_MIMES.len() + 1);
            sql_params.push(&id_str);
            for mime in nagori_core::SUPPORTED_IMAGE_MIMES {
                sql_params.push(mime);
            }
            conn.query_row(&sql, sql_params.as_slice(), |row| {
                let blob: Option<Vec<u8>> = row.get(0)?;
                let mime: Option<String> = row.get(1)?;
                Ok(blob.zip(mime))
            })
            .optional()
            .map_err(storage_err)
            .map(Option::flatten)
        })
        .await
    }
}

#[async_trait]
impl EntryRepository for SqliteStore {
    async fn insert(&self, entry: ClipboardEntry) -> Result<EntryId> {
        // Snapshot truncation state before moving the entry into the
        // blocking closure so the audit row can be written from the async
        // context (the in-transaction path can't reach `AuditLog::record`,
        // which itself acquires a fresh connection).
        let truncated = ngram_input_was_truncated(&entry.search.normalized_text);
        let stored_id = self
            .run_blocking(move |store| insert_entry_blocking(&store, &entry))
            .await?;
        if truncated {
            let detail = format!("cap_chars={MAX_NGRAM_INPUT_CHARS}");
            if let Err(err) = self
                .record("ngram_truncated", Some(stored_id), Some(&detail))
                .await
            {
                tracing::warn!(error = %err, "audit_record_failed");
            }
        }
        Ok(stored_id)
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
            .map_err(storage_err)
        })
        .await
    }

    async fn update_metadata(&self, id: EntryId, metadata: EntryMetadata) -> Result<()> {
        self.run_blocking(move |store| {
            let representation_set_hash = metadata.representation_set_hash.as_ref().map_or_else(
                || metadata.content_hash.value.clone(),
                |hash| hash.value.clone(),
            );
            let changed = {
                let conn = store.conn()?;
                conn.execute(
                    "UPDATE entries
                     SET source_app_name = ?1, source_bundle_id = ?2, source_executable_path = ?3,
                         content_hash = ?4, representation_set_hash = ?5,
                         use_count = ?6, updated_at = ?7, last_used_at = ?8
                     WHERE id = ?9 AND deleted_at IS NULL",
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
                        representation_set_hash,
                        metadata.use_count,
                        format_time(metadata.updated_at)?,
                        format_opt_time(metadata.last_used_at)?,
                        id.to_string(),
                    ],
                )
                .map_err(storage_err)?
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
            let tx = conn.transaction().map_err(storage_err)?;
            let changed = tx
                .execute(
                    "UPDATE entries SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                    params![now, id.to_string()],
                )
                .map_err(storage_err)?;
            if changed == 0 {
                return Err(AppError::NotFound);
            }
            delete_search_rows(&tx, &id.to_string())?;
            tx.commit().map_err(storage_err)?;
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
            // Hard `LIMIT` so a token-authed local client (or the daemon's
            // own UI) can never trigger an unbounded DB scan / `Vec`
            // allocation / JSON serialisation just by pinning more rows
            // than `MAX_READ_LIMIT`. The IPC response cap in `server.rs`
            // runs *after* serialisation, so without a SQL-side limit the
            // daemon would still pay the full allocation cost before
            // rejecting the response.
            let mut stmt = conn
                .prepare_cached(
                    "SELECT e.*, d.title, d.preview, d.normalized_text, d.language
                     FROM entries e
                     LEFT JOIN search_documents d ON d.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.pinned = 1
                       AND e.sensitivity != 'blocked'
                     ORDER BY e.updated_at DESC
                     LIMIT ?1",
                )
                .map_err(storage_err)?;
            let entries = stmt
                .query_map([MAX_READ_LIMIT as i64], row_to_entry)
                .map_err(storage_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage_err)?;
            Ok(entries)
        })
        .await
    }

    async fn list_representations(
        &self,
        id: EntryId,
    ) -> Result<Vec<StoredClipboardRepresentation>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // Role precedence keeps the replay order stable even if a future
            // schema relaxes the per-role ordinal monotonicity invariant.
            let mut stmt = conn
                .prepare_cached(
                    "SELECT r.role, r.mime_type, r.ordinal, r.text_content, r.payload_blob
                     FROM entry_representations r
                     JOIN entries e ON e.id = r.entry_id
                     WHERE e.id = ?1 AND e.deleted_at IS NULL
                     ORDER BY
                         CASE r.role
                             WHEN 'primary' THEN 0
                             WHEN 'plain_fallback' THEN 1
                             WHEN 'alternative' THEN 2
                             ELSE 3
                         END,
                         r.ordinal ASC",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![id.to_string()], |row| {
                    let role: String = row.get(0)?;
                    let mime: String = row.get(1)?;
                    let ordinal: i64 = row.get(2)?;
                    let text: Option<String> = row.get(3)?;
                    let blob: Option<Vec<u8>> = row.get(4)?;
                    Ok((role, mime, ordinal, text, blob))
                })
                .map_err(storage_err)?;
            let mut out = Vec::new();
            for row in rows {
                let (role_str, mime, ordinal, text, blob) = row.map_err(storage_err)?;
                let role = RepresentationRole::from_db_str(&role_str).ok_or_else(|| {
                    AppError::storage(format!("unknown representation role: {role_str}"))
                })?;
                let ordinal = u32::try_from(ordinal).map_err(|err| {
                    AppError::storage(format!("representation ordinal out of range: {err}"))
                })?;
                let data = decode_representation_payload(&mime, text, blob)?;
                out.push(StoredClipboardRepresentation {
                    role,
                    mime_type: mime,
                    ordinal,
                    data,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn list_representation_summaries(
        &self,
        ids: &[EntryId],
    ) -> Result<HashMap<EntryId, Vec<RepresentationSummary>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_strings: Vec<String> = ids.iter().map(EntryId::to_string).collect();
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // One round-trip for the whole batch — reads only the columns
            // the DTO needs (no blob / text_content), so even a 200-row
            // palette refresh stays cheap. Role precedence + ordinal match
            // `list_representations` so badges and the "preserved formats"
            // row reflect the same ordering the copy-back path will replay.
            let mut sql = String::from(
                "SELECT r.entry_id, r.role, r.mime_type, r.byte_count \
                 FROM entry_representations r \
                 JOIN entries e ON e.id = r.entry_id \
                 WHERE e.deleted_at IS NULL AND r.entry_id IN (",
            );
            for idx in 0..id_strings.len() {
                if idx > 0 {
                    sql.push(',');
                }
                sql.push('?');
            }
            sql.push_str(
                ") ORDER BY \
                 r.entry_id, \
                 CASE r.role \
                     WHEN 'primary' THEN 0 \
                     WHEN 'plain_fallback' THEN 1 \
                     WHEN 'alternative' THEN 2 \
                     ELSE 3 \
                 END, \
                 r.ordinal ASC",
            );
            let mut stmt = conn.prepare(&sql).map_err(storage_err)?;
            let params: Vec<&dyn ToSql> = id_strings.iter().map(|s| s as &dyn ToSql).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                    let entry_id_str: String = row.get(0)?;
                    let role: String = row.get(1)?;
                    let mime: String = row.get(2)?;
                    let byte_count: i64 = row.get(3)?;
                    Ok((entry_id_str, role, mime, byte_count))
                })
                .map_err(storage_err)?;
            let mut out: HashMap<EntryId, Vec<RepresentationSummary>> = HashMap::new();
            for row in rows {
                let (entry_id_str, role_str, mime, byte_count) = row.map_err(storage_err)?;
                let entry_id = EntryId::from_str(&entry_id_str).map_err(|err| {
                    AppError::storage(format!("invalid entry_id in summary row: {err}"))
                })?;
                let role = RepresentationRole::from_db_str(&role_str).ok_or_else(|| {
                    AppError::storage(format!("unknown representation role: {role_str}"))
                })?;
                let byte_count = u64::try_from(byte_count).map_err(|err| {
                    AppError::storage(format!("representation byte_count out of range: {err}"))
                })?;
                out.entry(entry_id)
                    .or_default()
                    .push(RepresentationSummary {
                        role,
                        mime_type: mime,
                        byte_count,
                    });
            }
            Ok(out)
        })
        .await
    }

    async fn list_file_path_sets(&self, ids: &[EntryId]) -> Result<HashMap<EntryId, Vec<String>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_strings: Vec<String> = ids.iter().map(EntryId::to_string).collect();
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            // Gate on the *canonical* `sensitivity` column, never a value the
            // caller supplied: only `Public` / `Unknown` rows are admitted,
            // mirroring `is_text_safe_for_default_output`. Sensitive rows are
            // filtered out in SQL so their `content_json` (and the raw paths
            // inside it) is never even read here. Restricting to `file_list`
            // keeps the batch to the rows a file summary applies to.
            let mut sql = String::from(
                "SELECT id, content_json FROM entries \
                 WHERE deleted_at IS NULL AND content_kind = 'file_list' \
                 AND sensitivity IN ('public', 'unknown') AND id IN (",
            );
            for idx in 0..id_strings.len() {
                if idx > 0 {
                    sql.push(',');
                }
                sql.push('?');
            }
            sql.push(')');
            let mut stmt = conn.prepare(&sql).map_err(storage_err)?;
            let params: Vec<&dyn ToSql> = id_strings.iter().map(|s| s as &dyn ToSql).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                    let entry_id_str: String = row.get(0)?;
                    let content_json: String = row.get(1)?;
                    Ok((entry_id_str, content_json))
                })
                .map_err(storage_err)?;
            let mut out: HashMap<EntryId, Vec<String>> = HashMap::new();
            for row in rows {
                let (entry_id_str, content_json) = row.map_err(storage_err)?;
                let entry_id = EntryId::from_str(&entry_id_str).map_err(|err| {
                    AppError::storage(format!("invalid entry_id in file-summary row: {err}"))
                })?;
                let content: ClipboardContent =
                    serde_json::from_str(&content_json).map_err(json_err)?;
                // The `content_kind = 'file_list'` filter should make every row
                // a `FileList`, but decode defensively rather than unwrap a
                // mismatched variant from a hand-edited / corrupt row.
                if let ClipboardContent::FileList(files) = content {
                    out.insert(entry_id, files.paths);
                }
            }
            Ok(out)
        })
        .await
    }
}

/// Map a representation row's `(mime, text_content, payload_blob)` triple
/// back to the in-memory [`RepresentationDataRef`] shape produced by the
/// capture pipeline. The schema CHECK enforces that exactly one of
/// `text_content` / `payload_blob` is set, and the MIME tells us whether
/// a text row was originally a `FilePaths` list (`text/uri-list`) or a
/// plain/HTML/RTF inline text payload.
fn decode_representation_payload(
    mime: &str,
    text: Option<String>,
    blob: Option<Vec<u8>>,
) -> Result<RepresentationDataRef> {
    match (text, blob) {
        (Some(text), None) => {
            if mime.eq_ignore_ascii_case("text/uri-list") {
                Ok(RepresentationDataRef::FilePaths(
                    nagori_core::decode_file_paths(&text),
                ))
            } else {
                Ok(RepresentationDataRef::InlineText(text))
            }
        }
        (None, Some(bytes)) => Ok(RepresentationDataRef::DatabaseBlob(bytes)),
        (Some(_), Some(_)) | (None, None) => Err(AppError::storage(
            "entry_representations row violated text_content/payload_blob CHECK".to_owned(),
        )),
    }
}

#[allow(clippy::too_many_lines)]
fn insert_entry_blocking(store: &SqliteStore, entry: &ClipboardEntry) -> Result<EntryId> {
    let requested_id = entry.id;
    let content_hash = entry.metadata.content_hash.value.clone();
    let representation_set_hash = entry
        .metadata
        .representation_set_hash
        .as_ref()
        .map_or_else(|| content_hash.clone(), |hash| hash.value.clone());
    let updated_at = format_time(entry.metadata.updated_at)?;
    let created_at = format_time(entry.metadata.created_at)?;
    let mut doc = entry.search.clone();
    // Extract image bytes before serialising. `pending_bytes` is
    // `#[serde(skip)]` so the JSON body never grows by the blob size —
    // image bytes live in `entry_representations.payload_blob` and are
    // fetched lazily by the preview command. For non-image entries the
    // representation row carries the plain text in `text_content`.
    let (content_for_storage, primary_payload) = match &entry.content {
        ClipboardContent::Image(img) => {
            let bytes = img.pending_bytes.clone();
            let mime = img.mime_type.clone();
            let mut stripped = img.clone();
            stripped.pending_bytes = None;
            let payload = bytes.map(|bytes| PrimaryPayload::Bytes {
                mime: mime
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                bytes,
            });
            (ClipboardContent::Image(stripped), payload)
        }
        other => (
            other.clone(),
            other
                .plain_text()
                .map(|text| PrimaryPayload::Text(text.to_owned())),
        ),
    };
    let mut conn = store.conn()?;
    // `BEGIN IMMEDIATE` so the dedupe SELECT and the follow-up INSERT/UPDATE run
    // under the write lock from the start. With a `DEFERRED` transaction two
    // captures of the same `representation_set_hash` on different pool
    // connections could both read "no existing row" before either writes; the
    // second would then either insert a duplicate or, against the WAL snapshot,
    // fail with `SQLITE_BUSY_SNAPSHOT` and lose the capture. Acquiring the write
    // lock up front serialises the two: the loser waits (bounded by
    // `busy_timeout`), then its SELECT observes the winner's committed row and
    // takes the UPDATE (dedupe) branch instead of racing the INSERT.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_err)?;
    // Resolve dedupe explicitly via SELECT-then-INSERT/UPDATE rather than
    // `INSERT ... ON CONFLICT(representation_set_hash) WHERE deleted_at IS NULL`,
    // because conflict resolution against a partial unique index is
    // SQLite-version dependent.
    //
    // Key the lookup on `representation_set_hash` rather than `content_hash`
    // so two snapshots with the same primary text but different
    // HTML/RTF/file-list alternatives land in distinct rows. Otherwise the
    // later capture would silently overwrite the earlier row's alternatives.
    let existing = tx
        .query_row(
            "SELECT id FROM entries WHERE representation_set_hash = ?1 AND deleted_at IS NULL",
            params![representation_set_hash],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_err)?;
    let stored_id_str = if let Some(existing) = existing {
        // Identical `representation_set_hash` implies the rep set is
        // byte-for-byte identical, so the reps don't need to be replaced.
        // Refresh source/sensitivity and bump timestamps so the dedupe
        // record reflects the most recent capture. Lifecycle flags
        // (`pinned`, `archived`, `use_count`, `last_used_at`, `expires_at`,
        // `deleted_at`) belong to the original row and are preserved.
        tx.execute(
            "UPDATE entries SET
                source_app_name = ?1,
                source_bundle_id = ?2,
                source_executable_path = ?3,
                sensitivity = ?4,
                created_at = ?5,
                updated_at = ?5
             WHERE id = ?6",
            params![
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
                sensitivity_to_str(entry.sensitivity),
                updated_at,
                existing,
            ],
        )
        .map_err(storage_err)?;
        existing
    } else {
        tx.execute(
            "INSERT INTO entries (
                id, content_kind, content_json, source_app_name,
                source_bundle_id, source_executable_path, content_hash,
                representation_set_hash, sensitivity, pinned, archived,
                use_count, created_at, updated_at, last_used_at, expires_at,
                deleted_at
             )
             VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
             )",
            params![
                requested_id.to_string(),
                kind_to_str(entry.content_kind()),
                serde_json::to_string(&content_for_storage).map_err(json_err)?,
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
                representation_set_hash,
                sensitivity_to_str(entry.sensitivity),
                bool_int(entry.lifecycle.pinned),
                bool_int(entry.lifecycle.archived),
                entry.metadata.use_count,
                created_at,
                updated_at,
                format_opt_time(entry.metadata.last_used_at)?,
                format_opt_time(entry.lifecycle.expires_at)?,
                format_opt_time(entry.lifecycle.deleted_at)?,
            ],
        )
        .map_err(storage_err)?;
        // When the capture pipeline filled `pending_representations` (the
        // snapshot path), persist every preserved rep so copy-back can
        // re-publish whatever flavour the source advertised. Otherwise fall
        // back to the legacy primary-only path used by CLI `add_text`,
        // synthesised entries, and post-classification Secret entries
        // (where the daemon clears the rep set to keep alternatives from
        // leaking around redaction).
        let entry_id_str = requested_id.to_string();
        if entry.pending_representations.is_empty() {
            if let Some(payload) = primary_payload.as_ref() {
                insert_primary_representation(&tx, &entry_id_str, payload, &created_at)?;
            }
        } else {
            insert_pending_representations(
                &tx,
                &entry_id_str,
                &entry.pending_representations,
                &created_at,
            )?;
        }
        entry_id_str
    };
    let stored_id =
        EntryId::from_str(&stored_id_str).map_err(|err| AppError::storage(err.to_string()))?;
    if stored_id != requested_id {
        doc.entry_id = stored_id;
    }
    upsert_document_blocking(&tx, &doc)?;
    tx.commit().map_err(storage_err)?;
    Ok(stored_id)
}

enum PrimaryPayload {
    Text(String),
    Bytes { mime: String, bytes: Vec<u8> },
}

fn insert_primary_representation(
    tx: &rusqlite::Transaction<'_>,
    entry_id: &str,
    payload: &PrimaryPayload,
    created_at: &str,
) -> Result<()> {
    let representation_id = format!("{entry_id}#primary");
    match payload {
        PrimaryPayload::Text(text) => {
            let byte_count = i64::try_from(text.len()).map_err(|err| {
                AppError::storage(format!(
                    "representation text byte count overflowed i64: {err}"
                ))
            })?;
            tx.execute(
                "INSERT INTO entry_representations (
                    id, entry_id, role, mime_type, platform_format, ordinal,
                    text_content, payload_blob, byte_count, created_at
                 )
                 VALUES (?1, ?2, 'primary', 'text/plain', NULL, 0,
                         ?3, NULL, ?4, ?5)",
                params![representation_id, entry_id, text, byte_count, created_at],
            )
            .map_err(storage_err)?;
        }
        PrimaryPayload::Bytes { mime, bytes } => {
            let byte_count = i64::try_from(bytes.len()).map_err(|err| {
                AppError::storage(format!("representation byte count overflowed i64: {err}"))
            })?;
            tx.execute(
                "INSERT INTO entry_representations (
                    id, entry_id, role, mime_type, platform_format, ordinal,
                    text_content, payload_blob, byte_count, created_at
                 )
                 VALUES (?1, ?2, 'primary', ?3, NULL, 0,
                         NULL, ?4, ?5, ?6)",
                params![
                    representation_id,
                    entry_id,
                    mime,
                    bytes,
                    byte_count,
                    created_at
                ],
            )
            .map_err(storage_err)?;
        }
    }
    Ok(())
}

fn insert_pending_representations(
    tx: &rusqlite::Transaction<'_>,
    entry_id: &str,
    reps: &[StoredClipboardRepresentation],
    created_at: &str,
) -> Result<()> {
    for rep in reps {
        let role = rep.role.as_str();
        let representation_id = format!("{entry_id}#{role}-{}", rep.ordinal);
        let ordinal = i64::from(rep.ordinal);
        let byte_count = i64::try_from(rep.byte_count()).map_err(|err| {
            AppError::storage(format!("representation byte count overflowed i64: {err}"))
        })?;
        match &rep.data {
            RepresentationDataRef::InlineText(text) => {
                tx.execute(
                    "INSERT INTO entry_representations (
                        id, entry_id, role, mime_type, platform_format, ordinal,
                        text_content, payload_blob, byte_count, created_at
                     )
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5,
                             ?6, NULL, ?7, ?8)",
                    params![
                        representation_id,
                        entry_id,
                        role,
                        rep.mime_type,
                        ordinal,
                        text,
                        byte_count,
                        created_at,
                    ],
                )
                .map_err(storage_err)?;
            }
            RepresentationDataRef::DatabaseBlob(bytes) => {
                tx.execute(
                    "INSERT INTO entry_representations (
                        id, entry_id, role, mime_type, platform_format, ordinal,
                        text_content, payload_blob, byte_count, created_at
                     )
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5,
                             NULL, ?6, ?7, ?8)",
                    params![
                        representation_id,
                        entry_id,
                        role,
                        rep.mime_type,
                        ordinal,
                        bytes,
                        byte_count,
                        created_at,
                    ],
                )
                .map_err(storage_err)?;
            }
            RepresentationDataRef::FilePaths(paths) => {
                // Encode as a JSON array under text_content so the schema's
                // "exactly one of text_content / payload_blob" CHECK is
                // satisfied and paths containing newlines survive the round
                // trip. `byte_count` (from `rep.byte_count()`) counts the same
                // encoded form, keeping retention math honest.
                let encoded = nagori_core::encode_file_paths(paths);
                tx.execute(
                    "INSERT INTO entry_representations (
                        id, entry_id, role, mime_type, platform_format, ordinal,
                        text_content, payload_blob, byte_count, created_at
                     )
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5,
                             ?6, NULL, ?7, ?8)",
                    params![
                        representation_id,
                        entry_id,
                        role,
                        rep.mime_type,
                        ordinal,
                        encoded,
                        byte_count,
                        created_at,
                    ],
                )
                .map_err(storage_err)?;
            }
        }
    }
    Ok(())
}
