use nagori_core::{
    AppError, EntryId, RecognizedText, Result, SearchDocument, Sensitivity, normalize_text,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{
    format_time, json_err, parse_sensitivity_strict, sensitivity_rank, sensitivity_to_str,
    storage_err,
};
use super::maintenance::checkpoint_truncate_after_purge;
use super::search::upsert_document_blocking;

/// An image entry the background recognizer has not processed for its
/// current bytes under the current content policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTextRecognition {
    pub entry_id: EntryId,
    pub content_hash: String,
}

/// What recognizing one image produced, already shaped by the privacy policy.
#[derive(Debug, Clone, PartialEq)]
pub enum TextRecognitionOutcome {
    /// The image was read. `text` is what the image's search document
    /// indexes (redacted when the recognized text classified as private or
    /// secret), and `raise_to` lifts the entry's sensitivity when the text
    /// showed the image itself holds such content.
    Recognized {
        text: RecognizedText,
        raise_to: Option<Sensitivity>,
    },
    /// The recognizer could not read the image; recorded so the background
    /// indexer does not retry it on every pass.
    Failed,
}

/// The `WHERE` fragment for an image entry that may be read: live, an
/// image, still holding the bytes `content_hash` names, and not masked.
const READABLE_IMAGE_SQL: &str = "e.id = ?1 AND e.deleted_at IS NULL
       AND e.content_kind = 'image' AND e.content_hash = ?2
       AND e.sensitivity IN ('public', 'unknown')";

impl SqliteStore {
    /// Live Public / Unknown image entries without a recognition row for
    /// their current bytes and the current content policy (`policy_hash`),
    /// newest first. Masked images are never listed: their text must not
    /// reach the index.
    pub async fn text_recognition_pending(
        &self,
        limit: usize,
        policy_hash: String,
    ) -> Result<Vec<PendingTextRecognition>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT e.id, e.content_hash FROM entries e
                     LEFT JOIN entry_text_recognition r ON r.entry_id = e.id
                     WHERE e.deleted_at IS NULL
                       AND e.content_kind = 'image'
                       AND e.sensitivity IN ('public', 'unknown')
                       AND (r.entry_id IS NULL
                            OR r.content_hash != e.content_hash
                            OR r.policy_hash != ?2)
                     ORDER BY e.created_at DESC
                     LIMIT ?1",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![limit, policy_hash], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(storage_err)?;
            rows.map(|row| {
                let (id, content_hash) = row.map_err(storage_err)?;
                Ok(PendingTextRecognition {
                    entry_id: parse_id(&id)?,
                    content_hash,
                })
            })
            .collect()
        })
        .await
    }

    /// The primary image bytes of an entry, but only while it may be read
    /// (see [`READABLE_IMAGE_SQL`]): the readability check and the read are
    /// one statement, so an image masked after it was listed as pending is
    /// never handed to the recognizer.
    pub async fn readable_image_payload(
        &self,
        entry_id: EntryId,
        content_hash: String,
    ) -> Result<Option<Vec<u8>>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                &format!(
                    "SELECT r.payload_blob FROM entry_representations r
                     JOIN entries e ON e.id = r.entry_id
                     WHERE {READABLE_IMAGE_SQL}
                       AND r.role = 'primary' AND r.payload_blob IS NOT NULL
                     ORDER BY r.ordinal LIMIT 1"
                ),
                params![entry_id.to_string(), content_hash],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(storage_err)
        })
        .await
    }

    /// Record a recognition outcome for an image and, when it was read, index
    /// its text so the image is found by what it shows.
    ///
    /// Skipped (returns `false`) when the entry is gone, its bytes changed
    /// since `content_hash` was read, or it is no longer Public / Unknown —
    /// the outcome would describe content the entry no longer holds or must
    /// not be indexed — and when a `Failed` outcome would replace a
    /// recognition already stored for the same bytes and policy (a timed-out
    /// duplicate read must not leave indexed text behind a `failed` row). A
    /// `raise_to` only ever lifts the stored sensitivity (see
    /// [`Self::raise_image_sensitivity`]).
    pub async fn record_text_recognition(
        &self,
        entry_id: EntryId,
        content_hash: String,
        policy_hash: String,
        outcome: TextRecognitionOutcome,
    ) -> Result<bool> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let id = entry_id.to_string();
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let current = tx
                .query_row(
                    &format!(
                        "SELECT e.sensitivity, d.title, d.preview, d.language
                         FROM entries e LEFT JOIN search_documents d ON d.entry_id = e.id
                         WHERE {READABLE_IMAGE_SQL}"
                    ),
                    params![id, content_hash],
                    |row| {
                        Ok((
                            parse_sensitivity_strict(&row.get::<_, String>(0)?)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(storage_err)?;
            let Some((sensitivity, title, preview, language)) = current else {
                return Ok(false);
            };
            let (status, text, lines_json, raise_to) = match &outcome {
                TextRecognitionOutcome::Recognized { text, raise_to } => (
                    "recognized",
                    text.text(),
                    serde_json::to_string(&text.lines).map_err(json_err)?,
                    *raise_to,
                ),
                TextRecognitionOutcome::Failed => {
                    let already_read = tx
                        .query_row(
                            "SELECT 1 FROM entry_text_recognition
                             WHERE entry_id = ?1 AND content_hash = ?2
                               AND policy_hash = ?3 AND status = 'recognized'",
                            params![id, content_hash, policy_hash],
                            |_| Ok(()),
                        )
                        .optional()
                        .map_err(storage_err)?;
                    if already_read.is_some() {
                        return Ok(false);
                    }
                    ("failed", String::new(), "[]".to_owned(), None)
                }
            };
            tx.execute(
                "INSERT INTO entry_text_recognition
                    (entry_id, content_hash, policy_hash, status, text, lines_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(entry_id) DO UPDATE SET
                    content_hash = excluded.content_hash,
                    policy_hash = excluded.policy_hash,
                    status = excluded.status,
                    text = excluded.text,
                    lines_json = excluded.lines_json,
                    created_at = excluded.created_at",
                params![id, content_hash, policy_hash, status, text, lines_json, now],
            )
            .map_err(storage_err)?;
            // A failure indexes nothing, which also takes back any text an
            // earlier read under another policy left in the document.
            let normalized_text = normalize_text(&text);
            let doc = SearchDocument {
                entry_id,
                title,
                preview: preview.unwrap_or_default(),
                tokens: normalized_text
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect(),
                normalized_text,
                language,
            };
            upsert_document_blocking(&tx, &doc)?;
            if let Some(raised) = raise_to {
                raise_sensitivity_blocking(&tx, &id, sensitivity, raised)?;
            }
            tx.commit().map_err(storage_err)?;
            Ok(true)
        })
        .await
    }

    /// Lift a readable image's sensitivity to `raised` (never lowering it)
    /// and drop its cached thumbnail, for an image whose recognized text
    /// showed it holds private or secret content. Applies whether or not the
    /// text itself is stored. Returns whether the sensitivity changed.
    pub async fn raise_image_sensitivity(
        &self,
        entry_id: EntryId,
        content_hash: String,
        raised: Sensitivity,
    ) -> Result<bool> {
        self.run_blocking(move |store| {
            let id = entry_id.to_string();
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let current = tx
                .query_row(
                    &format!("SELECT e.sensitivity FROM entries e WHERE {READABLE_IMAGE_SQL}"),
                    params![id, content_hash],
                    |row| parse_sensitivity_strict(&row.get::<_, String>(0)?),
                )
                .optional()
                .map_err(storage_err)?;
            let Some(sensitivity) = current else {
                return Ok(false);
            };
            let changed = raise_sensitivity_blocking(&tx, &id, sensitivity, raised)?;
            tx.commit().map_err(storage_err)?;
            Ok(changed)
        })
        .await
    }

    /// The recognized text of a live, readable image entry, if it was read
    /// under the current content policy (`policy_hash`).
    pub async fn get_text_recognition(
        &self,
        entry_id: EntryId,
        policy_hash: String,
    ) -> Result<Option<RecognizedText>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let lines = conn
                .query_row(
                    "SELECT r.lines_json FROM entry_text_recognition r
                     JOIN entries e ON e.id = r.entry_id
                     WHERE r.entry_id = ?1 AND r.status = 'recognized'
                       AND r.content_hash = e.content_hash AND r.policy_hash = ?2
                       AND e.deleted_at IS NULL
                       AND e.sensitivity IN ('public', 'unknown')",
                    params![entry_id.to_string(), policy_hash],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_err)?;
            lines
                .map(|lines| {
                    Ok(RecognizedText {
                        lines: serde_json::from_str(&lines).map_err(json_err)?,
                    })
                })
                .transpose()
        })
        .await
    }

    /// Forget recognized text and take it back out of the image search
    /// documents: every row when `keep_policy` is `None` (the user turned
    /// image text search off), or the rows shaped under another policy than
    /// `keep_policy` (a policy edit; those images are read again). Returns
    /// how many rows were removed. Freed pages are truncated out of the WAL
    /// like any other purge, so the text does not linger there.
    pub async fn clear_text_recognition(&self, keep_policy: Option<String>) -> Result<usize> {
        self.run_blocking(move |store| {
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            let docs = {
                let mut stmt = tx
                    .prepare(
                        "SELECT r.entry_id, d.title, d.preview, d.language
                         FROM entry_text_recognition r
                         LEFT JOIN search_documents d ON d.entry_id = r.entry_id
                         WHERE ?1 IS NULL OR r.policy_hash != ?1",
                    )
                    .map_err(storage_err)?;
                stmt.query_map(params![keep_policy], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(storage_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err)?
            };
            // Whatever the row's status, the document goes back to the empty
            // text an image is captured with.
            for (entry_id, title, preview, language) in &docs {
                let doc = SearchDocument {
                    entry_id: parse_id(entry_id)?,
                    title: title.clone(),
                    preview: preview.clone().unwrap_or_default(),
                    normalized_text: String::new(),
                    tokens: Vec::new(),
                    language: language.clone(),
                };
                upsert_document_blocking(&tx, &doc)?;
            }
            tx.execute(
                "DELETE FROM entry_text_recognition WHERE ?1 IS NULL OR policy_hash != ?1",
                params![keep_policy],
            )
            .map_err(storage_err)?;
            tx.commit().map_err(storage_err)?;
            checkpoint_truncate_after_purge(&conn, docs.len());
            Ok(docs.len())
        })
        .await
    }

    /// Whether any recognition row exists, so a disabled indexer can tell
    /// cheaply that there is nothing to clear.
    pub async fn has_text_recognition(&self) -> Result<bool> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM entry_text_recognition)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_err)
        })
        .await
    }
}

fn raise_sensitivity_blocking(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    current: Sensitivity,
    raised: Sensitivity,
) -> Result<bool> {
    if sensitivity_rank(raised) <= sensitivity_rank(current) {
        return Ok(false);
    }
    tx.execute(
        "UPDATE entries SET sensitivity = ?1 WHERE id = ?2",
        params![sensitivity_to_str(raised), id],
    )
    .map_err(storage_err)?;
    tx.execute(
        "DELETE FROM entry_thumbnails WHERE entry_id = ?1",
        params![id],
    )
    .map_err(storage_err)?;
    Ok(true)
}

fn parse_id(id: &str) -> Result<EntryId> {
    id.parse()
        .map_err(|err: uuid::Error| AppError::storage(err.to_string()))
}
