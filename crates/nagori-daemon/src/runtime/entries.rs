//! Clipboard-entry CRUD: capture, copy/paste, listing, deletion, pinning.

use nagori_core::{
    AppError, AuditLog, ClipboardContent, ClipboardEntry, EntryFactory, EntryId, EntryRepository,
    PasteFormat, PasteOption, Result, SecretAction, Sensitivity, SensitivityClassifier,
    SettingsRepository, build_paste_options, select_representation,
};

use super::NagoriRuntime;

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
        let classifier = SensitivityClassifier::try_new(settings)?;
        let classification = classifier.classify(&entry);
        entry.sensitivity = classification.sensitivity;
        if let Some(preview) = classification.redacted_preview {
            entry.search.preview = preview;
        }
        if matches!(entry.sensitivity, Sensitivity::Blocked) {
            let _ = self
                .store
                .record("entry_blocked", Some(entry.id), None)
                .await;
            return Err(AppError::Policy(
                "entry blocked by capture policy".to_owned(),
            ));
        }
        if matches!(
            classifier.apply_secret_handling(&mut entry, secret_handling),
            SecretAction::Drop,
        ) {
            let _ = self
                .store
                .record("secret_blocked", Some(entry.id), None)
                .await;
            return Err(AppError::Policy(
                "entry classified as secret and refused by secret_handling=block".to_owned(),
            ));
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

    pub async fn copy_entry_with_format(&self, id: EntryId, format: PasteFormat) -> Result<()> {
        let mut entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        if matches!(entry.sensitivity, Sensitivity::Blocked) {
            return Err(AppError::Policy(
                "blocked entries cannot be copied".to_owned(),
            ));
        }
        // Image bytes survive capture in an `entry_representations` row
        // whose `ImageContent.pending_bytes` is dropped on deserialise, so
        // hydrate the bytes before the platform writer needs them.
        if let ClipboardContent::Image(image) = &mut entry.content
            && image.pending_bytes.is_none()
            && let Some((bytes, mime)) = self.store.get_payload(id).await?
        {
            image.pending_bytes = Some(bytes);
            if image.mime_type.is_none() {
                image.mime_type = Some(mime);
            }
        }
        match format {
            PasteFormat::Preserve => {
                // Re-offer every stored representation so a receiver that
                // understands HTML / RTF / image bytes can pick the richest
                // representation the source originally advertised, while a
                // plain-text target still finds the matching `text/plain`
                // fallback. Adapters whose
                // `clipboard_multi_representation_write` capability is
                // `Unsupported` (e.g. `MemoryClipboard`, or any host
                // adapter not built into this binary) inherit the trait's
                // default impl, which delegates to `write_entry`.
                let representations = self.store.list_representations(id).await?;
                if representations.is_empty() {
                    self.clipboard.write_entry(&entry).await?;
                } else {
                    self.clipboard
                        .write_representations(&entry, &representations)
                        .await?;
                }
            }
            PasteFormat::PlainText => self.clipboard.write_plain(&entry).await?,
        }
        // The ranker scores by `metadata.use_count` (see nagori-search), so
        // bumping it changes which results win — drop cached hits before
        // *and* after the increment.
        self.invalidate_search_cache();
        self.store.increment_use_count(id).await?;
        self.invalidate_search_cache();
        Ok(())
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
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        if matches!(entry.sensitivity, Sensitivity::Blocked) {
            return Err(AppError::Policy(
                "blocked entries cannot be copied".to_owned(),
            ));
        }
        let representations = self.store.list_representations(id).await?;
        let representation = select_representation(&representations, mime).ok_or_else(|| {
            // Deliberately MIME- and payload-free: the error reaches the UI
            // toast, and the requested format is the only safe detail.
            AppError::InvalidInput(
                "the requested clipboard format is not available for this entry".to_owned(),
            )
        })?;
        self.clipboard
            .write_representation_exact(representation)
            .await?;
        // Same use-count bump + cache invalidation contract as the other
        // copy-back paths so the ranker reflects the re-paste.
        self.invalidate_search_cache();
        self.store.increment_use_count(id).await?;
        self.invalidate_search_cache();
        Ok(())
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
        // The clipboard write always runs so the user can hit ⌘V manually,
        // but we only synthesise the keystroke while `auto_paste_enabled`
        // is on. The palette command has a separate fallback path that
        // keeps the copy even when OS paste synthesis fails.
        let settings = self.store.get_settings().await?;
        self.copy_entry_with_format(id, format.unwrap_or(settings.paste_format_default))
            .await?;
        if settings.auto_paste_enabled {
            ensure_pasted(self.paste.paste_frontmost().await?)?;
        }
        Ok(())
    }

    pub async fn paste_frontmost(&self) -> Result<()> {
        ensure_pasted(self.paste.paste_frontmost().await?)
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
        self.invalidate_search_cache();
        self.store.mark_deleted(id).await?;
        self.invalidate_search_cache();
        Ok(())
    }

    /// Soft-delete every non-pinned entry. Returns the number of rows
    /// purged so callers can surface "cleared N entries" toasts.
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

/// Convert a `PasteResult` into an explicit success/failure.
///
/// `PasteController::paste_frontmost` reports OS-level outcomes via
/// `PasteResult { pasted, message }` and historically the daemon discarded
/// `pasted == false` as success. That hid both the unsupported-platform
/// branch (Noop on Linux/Windows) and any future "we tried but the OS
/// blocked it" path. We now treat `pasted=false` as a real failure and
/// promote `message` to the error so it surfaces in IPC / Tauri responses.
fn ensure_pasted(result: nagori_platform::PasteResult) -> Result<()> {
    if result.pasted {
        Ok(())
    } else {
        // `pasted == false` is the no-op controller branch (Noop on a host
        // without a wired paste adapter), i.e. synthetic paste is not
        // available here at all — classify it as such so the UI hint matches.
        Err(AppError::Paste {
            reason: nagori_core::PasteFailureReason::SynthUnsupported,
            message: result.message.unwrap_or_else(|| {
                "auto-paste did not run; OS paste controller reported pasted=false".to_owned()
            }),
        })
    }
}
