use nagori_core::{AppError, ClipboardEntry, EntryId, EntryLabel, Result};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;

use super::SqliteStore;
use super::convert::{format_time, json_err, kind_to_str, sensitivity_to_str, storage_err};
use super::search::upsert_document_blocking;

impl SqliteStore {
    /// Set or clear the user-given name of a live entry.
    ///
    /// # Errors
    ///
    /// [`AppError::NotFound`] when the entry is missing or deleted.
    pub async fn set_label(&self, id: EntryId, label: Option<EntryLabel>) -> Result<()> {
        self.run_blocking(move |store| {
            let now = format_time(OffsetDateTime::now_utc())?;
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            ensure_live(&tx, id)?;
            match label {
                Some(label) => {
                    let aliases = serde_json::to_string(&label.aliases).map_err(json_err)?;
                    tx.execute(
                        "INSERT INTO entry_labels (entry_id, name, aliases_json, search_text, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(entry_id) DO UPDATE SET
                             name = excluded.name,
                             aliases_json = excluded.aliases_json,
                             search_text = excluded.search_text,
                             updated_at = excluded.updated_at",
                        params![id.to_string(), label.name, aliases, label.search_text(), now],
                    )
                    .map_err(storage_err)?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM entry_labels WHERE entry_id = ?1",
                        params![id.to_string()],
                    )
                    .map_err(storage_err)?;
                }
            }
            tx.commit().map_err(storage_err)
        })
        .await
    }

    /// The user-given name of a live entry, if it has one.
    pub async fn get_label(&self, id: EntryId) -> Result<Option<EntryLabel>> {
        self.run_blocking(move |store| {
            let conn = store.conn()?;
            let row = conn
                .query_row(
                    "SELECT l.name, l.aliases_json FROM entry_labels l
                     JOIN entries e ON e.id = l.entry_id
                     WHERE l.entry_id = ?1 AND e.deleted_at IS NULL",
                    params![id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(storage_err)?;
            row.map(|(name, aliases)| {
                Ok(EntryLabel {
                    name,
                    aliases: serde_json::from_str(&aliases).map_err(json_err)?,
                })
            })
            .transpose()
        })
        .await
    }

    /// Replace a live entry's body with the content of `replacement`, keeping
    /// the entry's id, creation time, pin, name, use count and source.
    ///
    /// The stored representations, search document, thumbnail and embedding
    /// all derive from the old body, so they are rebuilt or dropped in the
    /// same transaction: copy-back then offers only the new body, and nothing
    /// of the old text stays searchable. `replacement` is a freshly built
    /// entry (classified and policy-checked by the caller); only its content,
    /// hashes, sensitivity, search document and primary representation are
    /// used.
    ///
    /// # Errors
    ///
    /// [`AppError::NotFound`] when the entry is missing or deleted, and
    /// [`AppError::InvalidInput`] when another live entry already holds the
    /// same content (the history keeps one row per distinct content).
    pub async fn replace_content(&self, id: EntryId, replacement: ClipboardEntry) -> Result<()> {
        self.run_blocking(move |store| {
            let content_hash = replacement.metadata.content_hash.value.clone();
            let representation_set_hash = replacement
                .metadata
                .representation_set_hash
                .as_ref()
                .map_or_else(|| content_hash.clone(), |hash| hash.value.clone());
            let content_json = serde_json::to_string(&replacement.content).map_err(json_err)?;
            let now = format_time(OffsetDateTime::now_utc())?;
            let id_str = id.to_string();
            let mut conn = store.conn()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_err)?;
            ensure_live(&tx, id)?;
            let duplicate = tx
                .query_row(
                    "SELECT 1 FROM entries
                     WHERE representation_set_hash = ?1 AND deleted_at IS NULL AND id != ?2",
                    params![representation_set_hash, id_str],
                    |_| Ok(()),
                )
                .optional()
                .map_err(storage_err)?;
            if duplicate.is_some() {
                return Err(AppError::InvalidInput(
                    "another item already holds this content".to_owned(),
                ));
            }
            tx.execute(
                "UPDATE entries SET
                    content_kind = ?1, content_json = ?2, content_hash = ?3,
                    representation_set_hash = ?4, sensitivity = ?5, updated_at = ?6
                 WHERE id = ?7",
                params![
                    kind_to_str(replacement.content_kind()),
                    content_json,
                    content_hash,
                    representation_set_hash,
                    sensitivity_to_str(replacement.sensitivity),
                    now,
                    id_str,
                ],
            )
            .map_err(storage_err)?;
            for table in [
                "entry_representations",
                "entry_thumbnails",
                "entry_embeddings",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE entry_id = ?1"),
                    params![id_str],
                )
                .map_err(storage_err)?;
            }
            if let Some(text) = replacement.content.plain_text() {
                let byte_count = i64::try_from(text.len()).map_err(|err| {
                    AppError::storage(format!("text byte count overflowed: {err}"))
                })?;
                tx.execute(
                    "INSERT INTO entry_representations (
                        id, entry_id, role, mime_type, platform_format, ordinal,
                        text_content, payload_blob, byte_count, created_at
                     )
                     VALUES (?1, ?2, 'primary', 'text/plain', NULL, 0, ?3, NULL, ?4, ?5)",
                    params![format!("{id_str}#primary"), id_str, text, byte_count, now],
                )
                .map_err(storage_err)?;
            }
            let mut doc = replacement.search;
            doc.entry_id = id;
            upsert_document_blocking(&tx, &doc)?;
            tx.commit().map_err(storage_err)
        })
        .await
    }
}

fn ensure_live(tx: &rusqlite::Transaction<'_>, id: EntryId) -> Result<()> {
    tx.query_row(
        "SELECT 1 FROM entries WHERE id = ?1 AND deleted_at IS NULL",
        params![id.to_string()],
        |_| Ok(()),
    )
    .optional()
    .map_err(storage_err)?
    .ok_or(AppError::NotFound)
}
