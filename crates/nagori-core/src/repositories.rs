use std::collections::HashMap;

use async_trait::async_trait;

use crate::{
    AppSettings, ClipboardEntry, EntryId, EntryMetadata, RepresentationSummary, Result,
    SearchDocument, StoredClipboardRepresentation,
};

#[async_trait]
pub trait EntryRepository: Send + Sync {
    async fn insert(&self, entry: ClipboardEntry) -> Result<EntryId>;
    async fn get(&self, id: EntryId) -> Result<Option<ClipboardEntry>>;
    async fn update_metadata(&self, id: EntryId, metadata: EntryMetadata) -> Result<()>;
    async fn mark_deleted(&self, id: EntryId) -> Result<()>;
    async fn list_recent(&self, limit: usize) -> Result<Vec<ClipboardEntry>>;
    async fn list_pinned(&self) -> Result<Vec<ClipboardEntry>>;

    /// Return every stored representation for `id`, ordered for replay by
    /// role precedence (`primary` → `plain_fallback` → `alternative`) and
    /// then by ordinal ascending. Returns an empty vector when the entry
    /// has no representation rows (synthesised entries, Secret rows whose
    /// representations were dropped before insert) or when the entry has
    /// been soft-deleted. Used by the copy-back path under
    /// `PasteFormat::Preserve` to re-publish every captured representation.
    async fn list_representations(&self, id: EntryId)
    -> Result<Vec<StoredClipboardRepresentation>>;

    /// Batched, payload-free counterpart to [`list_representations`] for the
    /// search / list-recent / list-pinned hot paths. Returns the
    /// `(role, mime, byte_count)` projection for every supplied id in one
    /// query so the DTO builders don't have to fan out N round-trips per
    /// palette refresh. The default implementation falls back to calling
    /// [`list_representations`] per id; the `SQLite` implementation overrides
    /// it with an `IN (...)` lookup that skips blob decoding.
    async fn list_representation_summaries(
        &self,
        ids: &[EntryId],
    ) -> Result<HashMap<EntryId, Vec<RepresentationSummary>>> {
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            let reps = self.list_representations(*id).await?;
            let summaries = reps
                .iter()
                .map(RepresentationSummary::from_stored)
                .collect();
            out.insert(*id, summaries);
        }
        Ok(out)
    }
}

#[async_trait]
pub trait SearchRepository: Send + Sync {
    async fn upsert_document(&self, doc: SearchDocument) -> Result<()>;
    async fn delete_document(&self, entry_id: EntryId) -> Result<()>;
}

#[async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_settings(&self) -> Result<AppSettings>;
    async fn save_settings(&self, settings: AppSettings) -> Result<()>;
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(
        &self,
        kind: &str,
        entry_id: Option<EntryId>,
        message: Option<&str>,
    ) -> Result<()>;
}
