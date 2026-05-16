use async_trait::async_trait;

use crate::{
    AppSettings, ClipboardEntry, EntryId, EntryMetadata, Result, SearchDocument,
    StoredClipboardRepresentation,
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
