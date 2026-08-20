//! Clipboard-entry CRUD: capture, copy/paste, listing, deletion, pinning.

use std::time::Instant;

use nagori_core::{
    AppError, AuditLog, ClipboardEntry, EntryFactory, EntryId, EntryRepository, PasteFormat,
    PasteOption, Result, SecretAction, SecretDropReason, Sensitivity, SensitivityClassifier,
    SensitivityReason, SettingsRepository, build_paste_options,
};

use crate::ipc_handler::result_code;

use super::{NagoriRuntime, elapsed_ms};

impl NagoriRuntime {
    pub async fn add_text(&self, text: String) -> Result<EntryId> {
        // Fail closed: if we can't load settings, refuse the write rather than
        // silently substituting defaults (that would re-enable a wider
        // denylist / weaker secret_handling than the user configured).
        let settings = self.store.get_settings().await?;
        if text.is_empty() {
            return Err(AppError::InvalidInput(
                "entry text must not be empty".to_owned(),
            ));
        }
        if text.len() > settings.max_entry_size_bytes {
            return Err(AppError::Policy(format!(
                "entry exceeds max_entry_size_bytes ({})",
                settings.max_entry_size_bytes
            )));
        }
        let mut entry = EntryFactory::from_text(text);
        let secret_handling = settings.secret_handling;
        let block_sensitive_captures = settings.block_sensitive_captures;
        let classifier = SensitivityClassifier::try_new(settings)?;
        let classification = classifier.classify(&entry);
        entry.sensitivity = classification.sensitivity;
        if let Some(preview) = classification.redacted_preview {
            entry.search.preview = preview;
        }
        // Stable `SensitivityReason::token()` CSV for every policy-drop audit
        // row below, matching the capture loop's format so the two ingest
        // paths stay greppable by the same tokens.
        let joined = classification
            .reasons
            .iter()
            .map(SensitivityReason::token)
            .collect::<Vec<_>>()
            .join(",");
        if matches!(entry.sensitivity, Sensitivity::Blocked) {
            let _ = self
                .store
                .record("entry_blocked", Some(entry.id), Some(&joined))
                .await;
            return Err(AppError::Policy(
                "entry blocked by capture policy".to_owned(),
            ));
        }
        if block_sensitive_captures
            && matches!(
                entry.sensitivity,
                Sensitivity::Private | Sensitivity::Secret
            )
        {
            let _ = self
                .store
                .record("sensitive_blocked", Some(entry.id), Some(&joined))
                .await;
            return Err(AppError::Policy(
                "entry classified as sensitive and refused by block_sensitive_captures=true"
                    .to_owned(),
            ));
        }
        if let SecretAction::Drop(reason) =
            classifier.apply_secret_handling(&mut entry, secret_handling)
        {
            match reason {
                SecretDropReason::BlockedBySetting => {
                    let _ = self
                        .store
                        .record("secret_blocked", Some(entry.id), Some(&joined))
                        .await;
                    return Err(AppError::Policy(
                        "entry classified as secret and refused by secret_handling=block"
                            .to_owned(),
                    ));
                }
                SecretDropReason::FullyRedacted => {
                    let _ = self
                        .store
                        .record("secret_redacted_dropped", Some(entry.id), Some(&joined))
                        .await;
                    return Err(AppError::Policy(
                        "entry was classified as secret and its entire body was redacted; \
                         nothing was stored"
                            .to_owned(),
                    ));
                }
            }
        }
        // Invalidate before *and* after: the pre-call closes the window
        // where a concurrent `search` could still serve a pre-insert hit
        // between commit and the post-call.
        self.invalidate_search_cache();
        let id = self.store.insert(entry).await?;
        self.invalidate_search_cache();
        Ok(id)
    }

    pub async fn copy_entry(&self, id: EntryId) -> Result<()> {
        self.copy_entry_with_format(id, PasteFormat::Preserve).await
    }

    /// Copy an entry back to the clipboard.
    ///
    /// `Preserve` re-offers every stored representation so a receiver that
    /// understands HTML / RTF / image bytes can pick the richest one the
    /// source originally advertised, while a plain-text target still finds the
    /// matching `text/plain` fallback; `PlainText` publishes only the plain
    /// body.
    ///
    /// Takes the clipboard lease for the length of the write, so this can
    /// never interleave with another publish or with a paste in flight. A
    /// caller that goes on to synthesise a paste must hold *one*
    /// [`Self::clipboard_lease`] across both steps instead of calling this —
    /// see [`crate::runtime::ClipboardLease`].
    pub async fn copy_entry_with_format(&self, id: EntryId, format: PasteFormat) -> Result<()> {
        self.clipboard_lease()
            .await
            .copy_entry_with_format(id, format)
            .await
            // Copy-only: no paste follows, so there is nothing for the
            // publish token to authorise.
            .map(drop)
    }

    /// Copy a single chosen representation of an entry back to the clipboard
    /// ("paste as PNG / plain text / files").
    ///
    /// Unlike [`Self::copy_entry_with_format`]'s `Preserve` path, which
    /// re-offers every stored representation, this publishes exactly the one
    /// the user picked and never falls back to the primary: a `mime` the
    /// entry doesn't hold (or the platform can't publish) is an error, so the
    /// user never silently gets a different format. The representation set is
    /// re-read here so a concurrent capture/eviction cannot make the picker's
    /// snapshot stale; `select_representation` resolves the request to the
    /// canonical (lowest role/ordinal) copy of that MIME.
    pub async fn copy_entry_representation(&self, id: EntryId, mime: &str) -> Result<()> {
        self.clipboard_lease()
            .await
            .copy_entry_representation(id, mime)
            .await
            // Copy-only: no paste follows, so there is nothing for the
            // publish token to authorise.
            .map(drop)
    }

    /// Join the text of several entries with newline separators, store the
    /// result as one entry, and publish it to the clipboard. Backs the
    /// palette's bulk copy action.
    ///
    /// Duplicate ids are collapsed, the selection is capped at
    /// [`nagori_core::MAX_COMBINED_COPY_ENTRIES`], and the join is refused
    /// (`InvalidInput`) once it would exceed `max_entry_size_bytes` — all
    /// before anything is stored or published, so an over-limit selection
    /// leaves neither the database nor the clipboard touched. Image / file-list entries and any
    /// non-`Public`/`Unknown` row are skipped; the multi-select UI surfaces
    /// the count of skipped entries to the user.
    ///
    /// Storing a history row for the combined text is deliberate, not a leak
    /// we tolerate: the joined text lands on the OS clipboard, and the capture
    /// loop would store it on its next tick regardless (this is a clipboard
    /// manager — there is no self-write suppression). Inserting it up front
    /// just makes that row appear immediately, with the shared sensitivity
    /// classification, and lets the later capture dedupe against it instead of
    /// producing a second copy.
    pub async fn copy_entries_combined(&self, ids: &[EntryId]) -> Result<()> {
        self.clipboard_lease()
            .await
            .copy_entries_combined(ids)
            .await
            // Copy-only: no paste follows, so there is nothing for the
            // publish token to authorise.
            .map(drop)
    }

    /// Enumerate the distinct representations the user can paste individually,
    /// in canonical order. Drives the desktop "paste as <format>" picker.
    ///
    /// A `Blocked` entry can never be copied, so it offers nothing. The set is
    /// re-read from storage (not the search snapshot) so the options reflect
    /// what is actually publishable right now.
    pub async fn list_paste_options(&self, id: EntryId) -> Result<Vec<PasteOption>> {
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        if matches!(entry.sensitivity, Sensitivity::Blocked) {
            return Ok(Vec::new());
        }
        let representations = self.store.list_representations(id).await?;
        Ok(build_paste_options(&representations))
    }

    pub async fn paste_entry(&self, id: EntryId, format: Option<PasteFormat>) -> Result<()> {
        // Wrap the body so every exit emits a completion event: the paste
        // path otherwise only surfaces scattered failure warns, leaving
        // ARCHITECTURE §17's grep recipes with no success signal.
        // `result_code` collapses to a static label, so the event never
        // carries clipboard content.
        let started = Instant::now();
        let result = self.paste_entry_inner(id, format).await;
        tracing::debug!(
            result_code = result_code(&result),
            elapsed_ms = elapsed_ms(started),
            "paste_entry"
        );
        result
    }

    async fn paste_entry_inner(&self, id: EntryId, format: Option<PasteFormat>) -> Result<()> {
        // The clipboard write always runs so the user can hit ⌘V manually,
        // but we only synthesise the keystroke while `auto_paste_enabled`
        // is on. The palette command has a separate fallback path that
        // keeps the copy even when OS paste synthesis fails.
        let settings = self.store.get_settings().await?;
        // One lease spans the publish and the synthesis, so no other request
        // can put its own clip on the clipboard in between — see
        // [`crate::runtime::ClipboardLease`].
        let mut lease = self.clipboard_lease().await;
        let publish = lease
            .copy_entry_with_format(id, format.unwrap_or(settings.paste_format_default))
            .await?;
        if settings.auto_paste_enabled {
            lease.paste_frontmost(publish).await?;
        }
        Ok(())
    }

    pub async fn list_recent(&self, limit: usize) -> Result<Vec<ClipboardEntry>> {
        self.store.list_recent(limit).await
    }

    pub async fn list_pinned(&self) -> Result<Vec<ClipboardEntry>> {
        self.store.list_pinned().await
    }

    pub async fn get_entry(&self, id: EntryId) -> Result<Option<ClipboardEntry>> {
        self.store.get(id).await
    }

    pub async fn delete_entry(&self, id: EntryId) -> Result<()> {
        let settings = self.store.get_settings().await?;
        if settings.permanent_delete_on_delete {
            return self.hard_delete_entry(id).await;
        }
        self.invalidate_search_cache();
        self.store.mark_deleted(id).await?;
        self.invalidate_search_cache();
        Ok(())
    }

    pub async fn hard_delete_entry(&self, id: EntryId) -> Result<()> {
        self.invalidate_search_cache();
        self.store.hard_delete_entry(id).await?;
        self.invalidate_search_cache();
        Ok(())
    }

    pub async fn purge_deleted_entries(&self) -> Result<usize> {
        self.invalidate_search_cache();
        let purged = self.store.purge_deleted().await?;
        self.invalidate_search_cache();
        Ok(purged)
    }

    /// Clear the history: hide every non-pinned entry now and reclaim the rows
    /// in the background. Returns the number of entries that left the palette
    /// so callers can surface "cleared N entries" toasts.
    ///
    /// This is the interactive clear every UI surface (tray, palette, `nagori
    /// clear`) goes through. The physical delete is deferred because it takes
    /// tens of seconds on a large history and readers stay on the pre-commit
    /// snapshot until it lands — the user would clear their clipboard and keep
    /// seeing it. [`Self::clear_non_pinned`] is the synchronous variant
    /// clear-on-quit needs.
    pub async fn clear_history(&self) -> Result<usize> {
        self.invalidate_search_cache();
        let hidden = self.store.tombstone_non_pinned().await?;
        self.invalidate_search_cache();
        // Kick the maintenance sweep so the rows this hid are physically gone
        // within seconds instead of sitting until the next periodic tick.
        self.request_maintenance();
        Ok(hidden)
    }

    /// Physically delete every non-pinned entry, blocking until the rows are
    /// gone. Used by clear-on-quit, which has no later moment to run a
    /// deferred purge in. Interactive callers want [`Self::clear_history`].
    pub async fn clear_non_pinned(&self) -> Result<usize> {
        self.invalidate_search_cache();
        let purged = self.store.clear_non_pinned().await?;
        self.invalidate_search_cache();
        Ok(purged)
    }

    pub async fn pin_entry(&self, id: EntryId, pinned: bool) -> Result<()> {
        // `recent_entries` hoists pinned rows to the top, so flipping the
        // pin bit reorders the empty-query result; the cache must drop hits
        // both before and after the storage write.
        self.invalidate_search_cache();
        self.store.set_pinned(id, pinned).await?;
        self.invalidate_search_cache();
        Ok(())
    }

    pub async fn get_payload(&self, id: EntryId) -> Result<Option<(Vec<u8>, String)>> {
        self.store.get_payload(id).await
    }
}
