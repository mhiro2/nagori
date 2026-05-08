use std::sync::Arc;
use std::time::{Duration, Instant};

use nagori_core::{
    AppError, AppSettings, AuditLog, ClipboardContent, ClipboardSequence, EntryFactory, EntryId,
    EntryRepository, Result, SecretAction, Sensitivity, SensitivityClassifier,
};
use nagori_platform::{ClipboardReader, WindowBehavior};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::search_cache::SharedSearchCache;

/// Minimum gap between two consecutive `AppError::Platform` warnings out of
/// the capture loop. The OS-level clipboard read can fail repeatedly (e.g.
/// after a permission revocation) at the polling cadence, which would flood
/// the log if we warned on every tick. One warn per minute is enough to make
/// the failure visible without burying everything else.
const PLATFORM_WARN_INTERVAL: Duration = Duration::from_mins(1);

pub struct CaptureLoop<R, E, A> {
    reader: R,
    entries: E,
    audit: A,
    settings: AppSettings,
    last_sequence: Option<ClipboardSequence>,
    window: Option<Arc<dyn WindowBehavior>>,
    last_platform_warn_at: Option<Instant>,
    search_cache: Option<SharedSearchCache>,
    /// `true` until the loop has observed and acted on its first sequence.
    /// When `capture_initial_clipboard_on_launch` is `false`, the first
    /// observed sequence is recorded as `last_sequence` and the body read is
    /// skipped, so whatever was already on the pasteboard at startup never
    /// reaches storage.
    pristine: bool,
}

impl<R, E, A> CaptureLoop<R, E, A>
where
    R: ClipboardReader,
    E: EntryRepository,
    A: AuditLog,
{
    pub const fn new(reader: R, entries: E, audit: A, settings: AppSettings) -> Self {
        Self {
            reader,
            entries,
            audit,
            settings,
            last_sequence: None,
            window: None,
            last_platform_warn_at: None,
            search_cache: None,
            pristine: true,
        }
    }

    /// Reset the dedup baseline so the next observed sequence is treated as
    /// fresh content. Useful after macOS sleep/wake when the pasteboard
    /// counter can lap silently and we'd otherwise skip a real change as a
    /// duplicate.
    pub fn reset_sequence_baseline(&mut self) {
        self.last_sequence = None;
    }

    fn note_capture_error(&mut self, err: &AppError) {
        if let AppError::Platform(_) = err {
            // Rate-limit so an OS-level read failure (revoked pasteboard
            // access, AppKit hiccup) gets one visible warn per minute
            // instead of being swallowed entirely.
            let now = Instant::now();
            let should_warn = self
                .last_platform_warn_at
                .is_none_or(|prev| now.duration_since(prev) >= PLATFORM_WARN_INTERVAL);
            if should_warn {
                warn!(error = %err, "capture_failed_platform");
                self.last_platform_warn_at = Some(now);
            }
        } else {
            warn!(error = %err, "capture_failed");
        }
    }

    #[must_use]
    pub fn with_window(mut self, window: Arc<dyn WindowBehavior>) -> Self {
        self.window = Some(window);
        self
    }

    /// Wire a [`SharedSearchCache`] so successful captures invalidate stale
    /// hits in front of `SearchService`. Without this, the runtime cache
    /// would keep serving pre-capture results until another mutation
    /// (delete / pin / clear) eventually flushed it.
    #[must_use]
    pub fn with_search_cache(mut self, cache: SharedSearchCache) -> Self {
        self.search_cache = Some(cache);
        self
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        self.settings = settings;
    }

    #[allow(clippy::too_many_lines)]
    pub async fn capture_once(&mut self) -> Result<Option<EntryId>> {
        if !self.settings.capture_enabled {
            return Ok(None);
        }

        // Detect the change cheaply via the pasteboard sequence first, then
        // capture the frontmost app *before* we incur the cost of reading
        // the clipboard body. On macOS `arboard::Clipboard::get_text` does
        // several pasteboard round-trips; if the user is fast enough to
        // switch apps between copy and our read, the frontmost we'd capture
        // afterwards would be the destination app rather than the source.
        // This lets the password-manager / app-denylist rules in
        // `SensitivityClassifier` actually fire for things like 1Password
        // ⌘C → ⌘Tab → paste flows.
        let sequence = self.reader.current_sequence().await?;
        if self.last_sequence.as_ref() == Some(&sequence) {
            return Ok(None);
        }
        // Honour the "skip whatever was on the clipboard before launch" flag
        // by anchoring `last_sequence` to the first sequence we observe and
        // bailing out without reading the body. Subsequent ticks behave
        // normally because `pristine` flips to `false`.
        if self.pristine && !self.settings.capture_initial_clipboard_on_launch {
            self.pristine = false;
            self.last_sequence = Some(sequence);
            return Ok(None);
        }
        self.pristine = false;
        let frontmost_source = if let Some(window) = &self.window {
            window
                .frontmost_app()
                .await
                .ok()
                .flatten()
                .map(|front| front.source)
        } else {
            None
        };

        let mut snapshot = self.reader.current_snapshot().await?;
        self.last_sequence = Some(snapshot.sequence.clone());
        if snapshot.source.is_none() {
            snapshot.source = frontmost_source;
        }

        let Some(mut entry) = EntryFactory::from_snapshot(snapshot) else {
            return Ok(None);
        };
        if !self.settings.capture_kinds.contains(&entry.content_kind()) {
            info!(kind = ?entry.content_kind(), "capture_skipped reason=kind_disabled");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("kind_disabled"))
                .await;
            return Ok(None);
        }
        // Image entries don't carry plain text, so size them by their byte
        // payload instead — otherwise the empty-text guard below silently
        // dropped every image snapshot and the README's image-capture promise
        // never reached storage.
        let payload_bytes = match &entry.content {
            ClipboardContent::Image(img) => img.byte_count,
            _ => entry.plain_text().map_or(0, str::len),
        };
        if payload_bytes == 0 {
            return Ok(None);
        }
        if payload_bytes > self.settings.max_entry_size_bytes {
            warn!(bytes = payload_bytes, "capture_skipped reason=oversized");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("oversized"))
                .await;
            return Ok(None);
        }
        // Fail closed if the persisted regex_denylist contains an
        // uncompilable pattern — silently dropping it would let secret
        // matches the user explicitly asked us to redact slip into history.
        let classifier = SensitivityClassifier::try_new(self.settings.clone())?;
        let classification = classifier.classify(&entry);
        entry.sensitivity = classification.sensitivity;
        if matches!(classification.sensitivity, Sensitivity::Blocked) {
            info!(reasons = ?classification.reasons, "entry_blocked");
            let _ = self
                .audit
                .record(
                    "entry_blocked",
                    Some(entry.id),
                    Some(&format!("{:?}", classification.reasons)),
                )
                .await;
            return Ok(None);
        }
        if let Some(preview) = classification.redacted_preview {
            entry.search.preview = preview;
        }
        if matches!(
            classifier.apply_secret_handling(&mut entry, self.settings.secret_handling),
            SecretAction::Drop,
        ) {
            info!(reasons = ?classification.reasons, "secret_blocked");
            let _ = self
                .audit
                .record(
                    "secret_blocked",
                    Some(entry.id),
                    Some(&format!("{:?}", classification.reasons)),
                )
                .await;
            return Ok(None);
        }

        // Invalidate before *and* after the insert. Without the pre-call,
        // a concurrent `runtime.search()` could lock the cache between
        // SQLite commit and our post-invalidate and serve a pre-insert hit
        // even though the new row is already durable.
        if let Some(cache) = &self.search_cache
            && let Ok(mut guard) = cache.lock()
        {
            guard.invalidate();
        }
        let id = self.entries.insert(entry).await?;
        info!(entry_id = %id, "entry_inserted");
        if let Some(cache) = &self.search_cache
            && let Ok(mut guard) = cache.lock()
        {
            guard.invalidate();
        }
        Ok(Some(id))
    }

    pub async fn run_polling(
        &mut self,
        interval: std::time::Duration,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(interval) => {
                    if let Err(err) = self.capture_once().await {
                        self.note_capture_error(&err);
                    }
                }
            }
        }
    }

    pub async fn run_polling_with_settings(
        &mut self,
        interval: std::time::Duration,
        mut settings_rx: watch::Receiver<AppSettings>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                changed = settings_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let next = settings_rx.borrow().clone();
                    self.update_settings(next);
                }
                () = tokio::time::sleep(interval) => {
                    if let Err(err) = self.capture_once().await {
                        self.note_capture_error(&err);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nagori_core::{AppSettings, EntryRepository, Sensitivity, settings::SecretHandling};
    use nagori_platform::{ClipboardWriter, MemoryClipboard};
    use nagori_storage::SqliteStore;

    use super::*;

    fn loop_for(
        clipboard: Arc<MemoryClipboard>,
        store: SqliteStore,
        settings: AppSettings,
    ) -> CaptureLoop<Arc<MemoryClipboard>, SqliteStore, SqliteStore> {
        CaptureLoop::new(clipboard, store.clone(), store, settings)
    }

    #[tokio::test]
    async fn capture_once_dedupes_repeated_clipboard_text() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

        // Empty clipboard text → no snapshot text → nothing inserted.
        assert!(loop_.capture_once().await.unwrap().is_none());

        clipboard
            .write_text("captured value alpha")
            .await
            .expect("clipboard write");
        let first = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("first capture should record an entry");

        // Same text → same sequence → skipped.
        assert!(loop_.capture_once().await.unwrap().is_none());

        clipboard
            .write_text("captured value bravo")
            .await
            .expect("clipboard write");
        let second = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("new text should record a new entry");
        assert_ne!(first, second);

        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn capture_once_skips_when_capture_disabled() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_enabled: false,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("ignored value")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_skips_disabled_content_kind_before_classification() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_kinds: std::iter::once(nagori_core::ContentKind::Image).collect(),
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("plain text should be ignored")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_skips_oversized_blocked_text() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        // Drop max_entry_size_bytes so any short clip is classified as
        // oversized and the capture loop must skip insertion.
        let settings = AppSettings {
            max_entry_size_bytes: 4,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("this is too long for the policy")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_drops_user_regex_match() {
        // Regex_denylist UI promises "Captures matching any pattern are
        // dropped" — so a UserRegex-matched clip must never reach SQLite,
        // not even with a redacted body. Regression for the original
        // behaviour where UserRegex classified as Private and the raw
        // text was persisted as `entry.content`.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("ticket INTERNAL-123 must stay local")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_blocks_secret_when_handling_is_block() {
        // SecretHandling::Block must drop Secret-tagged content entirely
        // (not just redact it). Regression for the original behaviour where
        // the secret_handling setting was ignored and every Secret payload
        // was persisted verbatim.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            secret_handling: SecretHandling::Block,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_redacts_secret_by_default() {
        // The default SecretHandling::StoreRedacted has to land a row whose
        // durable body is the redacted form. An exported DB must never
        // expose the raw token.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("redacted secret should be persisted");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(stored.sensitivity, Sensitivity::Secret);
        let body = stored.plain_text().expect("body").to_owned();
        assert!(
            !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
            "default secret_handling must not store the raw token: {body:?}",
        );
        assert!(body.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn capture_once_keeps_secret_full_when_opted_in() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            secret_handling: SecretHandling::StoreFull,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        let id = loop_.capture_once().await.unwrap().expect("entry id");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(
            stored.plain_text(),
            Some("token = ghp_abcdefghijklmnopqrstuvwxyz123456"),
        );
        // Even with the raw body retained, the search preview must still be
        // the redacted form so UI surfaces never leak the secret.
        assert!(stored.search.preview.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn capture_once_attributes_frontmost_app_to_snapshot() {
        // Regression for the polling race where we read the clipboard text
        // before grabbing frontmost — by the time `frontmost_app` was
        // queried, the user could have switched away and the password-
        // manager source attribution (which the denylist relies on) would
        // be lost. We now capture frontmost immediately after the sequence
        // change, before reading the (potentially slower) clipboard body.
        use async_trait::async_trait;
        use nagori_core::SourceApp;
        use nagori_platform::{FrontmostApp, WindowBehavior};

        #[derive(Default)]
        struct FixedFrontmost;

        #[async_trait]
        impl WindowBehavior for FixedFrontmost {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.agilebits.onepassword".to_owned()),
                        name: Some("1Password".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(FixedFrontmost));

        clipboard
            .write_text("safe-looking value")
            .await
            .expect("clipboard write");

        // 1Password is on the default app_denylist, so the entry must be
        // dropped (Sensitivity::Blocked) once the source attribution is
        // attached. If the frontmost weren't picked up, the entry would be
        // persisted as Public and the test would observe a stored row.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_persists_image_clipboard_entries() {
        // The capture loop must keep image snapshots flowing through to the
        // store even though they have no plain text — otherwise the
        // README's "Captures text/URL/image" promise quietly turns into
        // text-only and image rows never reach search/preview.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct ImageReader {
            bytes: Vec<u8>,
            mime: &'static str,
            // Pretend the user only just copied — read once then "stable" so
            // capture_once's sequence-dedup short-circuit does not fire on a
            // second tick within the same test.
            seq_called: Mutex<bool>,
        }

        #[async_trait]
        impl ClipboardReader for ImageReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot {
                    sequence: ClipboardSequence(ContentHash::sha256(&self.bytes).value),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: self.mime.to_owned(),
                        data: ClipboardData::Bytes(self.bytes.clone()),
                    }],
                })
            }

            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                let mut guard = self.seq_called.lock().unwrap();
                let _ = &*guard;
                *guard = true;
                Ok(ClipboardSequence(ContentHash::sha256(&self.bytes).value))
            }
        }

        let bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
        let reader = ImageReader {
            bytes: bytes.clone(),
            mime: "image/png",
            seq_called: Mutex::new(false),
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ =
            CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("image entry should be inserted");
        let stored = store.get(id).await.unwrap().expect("row");
        match &stored.content {
            ClipboardContent::Image(img) => {
                assert_eq!(img.byte_count, bytes.len());
                assert_eq!(img.mime_type.as_deref(), Some("image/png"));
            }
            other => panic!("expected Image content, got {other:?}"),
        }
        let payload = store.get_payload(id).await.unwrap();
        assert_eq!(payload, Some((bytes, "image/png".to_owned())));
    }

    #[tokio::test]
    async fn capture_once_skips_oversized_image_payloads() {
        // The size guard must be denominated in image byte_count for image
        // snapshots — pre-fix, the guard saw `text.len() == 0` and let any
        // image through regardless of payload size.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct ImageReader {
            bytes: Vec<u8>,
            seq_called: Mutex<bool>,
        }

        #[async_trait]
        impl ClipboardReader for ImageReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot {
                    sequence: ClipboardSequence(ContentHash::sha256(&self.bytes).value),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: "image/png".to_owned(),
                        data: ClipboardData::Bytes(self.bytes.clone()),
                    }],
                })
            }

            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                let mut guard = self.seq_called.lock().unwrap();
                let _ = &*guard;
                *guard = true;
                Ok(ClipboardSequence(ContentHash::sha256(&self.bytes).value))
            }
        }

        let bytes = vec![0u8; 256];
        let reader = ImageReader {
            bytes: bytes.clone(),
            seq_called: Mutex::new(false),
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            max_entry_size_bytes: 64,
            ..AppSettings::default()
        };
        let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_invalidates_attached_search_cache() {
        // The runtime serves repeat empty-query searches from
        // `RecentSearchCache`, but a successful capture must drop those
        // hits — otherwise newly captured clips would not surface in the
        // palette until some other mutation flushed the cache.
        use crate::search_cache::{CacheKey, RecentSearchCache};
        use nagori_core::{
            ContentKind, EntryId, RankReason, SearchFilters, SearchMode, SearchResult, Sensitivity,
        };
        use std::sync::{Arc, Mutex};
        use time::OffsetDateTime;

        let cache = Arc::new(Mutex::new(RecentSearchCache::default()));
        cache.lock().unwrap().put(
            CacheKey {
                normalized: String::new(),
                mode: SearchMode::Auto,
                recent_order: nagori_core::RecentOrder::ByRecency,
                limit: 10,
                filters: SearchFilters::default(),
            },
            vec![SearchResult {
                entry_id: EntryId::new(),
                score: 1.0,
                rank_reason: vec![RankReason::Recent],
                preview: String::new(),
                content_kind: ContentKind::Text,
                created_at: OffsetDateTime::now_utc(),
                pinned: false,
                sensitivity: Sensitivity::Public,
                source_app_name: None,
            }],
        );
        assert_eq!(cache.lock().unwrap().len(), 1);

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store,
            AppSettings::default(),
        )
        .with_search_cache(cache.clone());
        clipboard
            .write_text("captured value")
            .await
            .expect("clipboard write");

        loop_
            .capture_once()
            .await
            .expect("capture")
            .expect("entry inserted");

        assert!(
            cache.lock().unwrap().is_empty(),
            "successful capture must invalidate the attached search cache",
        );
    }

    #[tokio::test]
    async fn capture_once_skips_existing_clipboard_when_disabled_on_launch() {
        // capture_initial_clipboard_on_launch=false: whatever was on the
        // pasteboard before Nagori started must be ignored, but a *new*
        // clip after that point should still be captured.
        let clipboard = Arc::new(MemoryClipboard::new());
        clipboard
            .write_text("preexisting clipboard value")
            .await
            .expect("seed clipboard");
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_initial_clipboard_on_launch: false,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

        // First tick observes the existing sequence and discards it.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());

        // A user-initiated clip after launch must still flow through.
        clipboard
            .write_text("post launch clip")
            .await
            .expect("clipboard write");
        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("post-launch clip should be inserted");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(stored.plain_text(), Some("post launch clip"));
    }

    #[tokio::test]
    async fn update_settings_takes_effect_on_next_capture() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

        clipboard
            .write_text("first value")
            .await
            .expect("clipboard write");
        loop_
            .capture_once()
            .await
            .expect("capture")
            .expect("entry inserted");

        // Disable capture mid-flight; the next clipboard change must be
        // ignored even though the reader returns fresh content.
        loop_.update_settings(AppSettings {
            capture_enabled: false,
            ..AppSettings::default()
        });
        clipboard
            .write_text("second value")
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_none());

        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);
    }
}
