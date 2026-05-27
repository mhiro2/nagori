use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nagori_ai::{AiActionRegistry, AiProvider, MockAiProvider};
use nagori_core::{
    AiActionId, AppError, AppSettings, AuditLog, ClipboardContent, ClipboardEntry, EntryFactory,
    EntryId, EntryRepository, OnboardingSettings, PasteFormat, Result, SearchQuery, SearchResult,
    SecretAction, Sensitivity, SensitivityClassifier, SettingsRepository,
    settings::AiProviderSetting,
};
use nagori_ipc::IpcServerHealth;
use nagori_platform::{
    ClipboardWriter, MemoryClipboard, NoopPasteController, PasteController, PermissionCheckContext,
    PermissionChecker, PermissionKind, PermissionState, PermissionStatus, PlatformCapabilities,
    unsupported_capabilities,
};
use nagori_storage::SqliteStore;
use time::OffsetDateTime;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tracing::error;

use nagori_core::ThumbnailRecord;

use crate::health::{CaptureHealth, MaintenanceHealth, StartupHealth};
use crate::search_cache::{
    CacheKey, CacheLookup, SharedSearchCache, lock_or_recover, new_shared_cache,
};
use crate::thumbnails::{self, ThumbnailGate};

#[derive(Clone)]
pub struct NagoriRuntime {
    pub(crate) store: SqliteStore,
    clipboard: Arc<dyn ClipboardWriter>,
    paste: Arc<dyn PasteController>,
    ai: Arc<dyn AiProvider>,
    ai_registry: Arc<AiActionRegistry>,
    pub(crate) permissions: Option<Arc<dyn PermissionChecker>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    settings_tx: watch::Sender<AppSettings>,
    settings_rx: watch::Receiver<AppSettings>,
    pub(crate) socket_path: Arc<std::path::PathBuf>,
    /// Front-of-store LRU for recent search results. Hits skip the `SQLite`
    /// round-trip on the empty-query (`Recent`) and short-prefix paths;
    /// any corpus mutation invalidates it via [`Self::invalidate_search_cache`].
    search_cache: SharedSearchCache,
    /// Shared health snapshot of the background maintenance loop. The
    /// loop writes from `serve.rs` after each iteration; the IPC
    /// `Health` and `Doctor` handlers read it.
    pub(crate) maintenance_health: MaintenanceHealth,
    /// Shared one-shot health snapshot of the capture loop's pre-poll
    /// initialisation. Recorded by whichever process hosts the capture
    /// task (`serve.rs` for the daemon, `state.rs` for the desktop) and
    /// read by `nagori doctor` plus the desktop's gated "ready"
    /// notification.
    pub(crate) startup_health: StartupHealth,
    /// Shared health snapshot of the capture loop's per-tick outcomes.
    /// Updated from the process hosting the capture task (`serve.rs` for
    /// the daemon, `state.rs` for the desktop); read by the IPC `Health`
    /// and `Doctor` handlers so dashboards can distinguish "retention is
    /// wedged" from "every clip is being dropped".
    pub(crate) capture_health: CaptureHealth,
    /// Shared handle for the IPC server's per-handler panic counter.
    /// The accept loop in `serve.rs` increments it via
    /// `IpcServerHealth::record_panic` (through `observe_handler_outcome`);
    /// the IPC `Health` and `Doctor` handlers read it so a panicking
    /// dispatcher is visible in `nagori doctor` / `nagori health`
    /// instead of silently swallowed by `JoinSet::join_next()`.
    pub(crate) ipc_health: IpcServerHealth,
    /// Static report of what the host adapter can do. Populated by the
    /// caller (typically `nagori-platform-native::build_native_runtime`)
    /// so the daemon doesn't have to take a dep on the per-OS crates;
    /// the IPC `Capabilities` handler clones it on demand. Wrapped in
    /// `Arc` to keep `NagoriRuntime: Clone` cheap.
    capabilities: Arc<PlatformCapabilities>,
    /// Deduplicator for in-flight thumbnail generation. Frontend layouts
    /// often fire several `nagori-image://thumb/<id>` requests for the
    /// same row in quick succession; the gate keeps a single decode in
    /// flight per entry id so a burst of misses doesn't spawn redundant
    /// blocking-pool work or race two `put_thumbnail` writes.
    thumbnail_gate: ThumbnailGate,
    /// Rate limiter + result cache for `fetch_latest_release_version`.
    /// The doctor handler can be invoked at arbitrary cadence (CLI poll,
    /// dashboard tick), so without this every call would issue a fresh
    /// HTTP request to GitHub — flapping networks would hammer the API
    /// and a denylist response would cascade across every probe. The
    /// state caches the last successful tag, gates retries with a 24h
    /// floor, and hard-disables further attempts after a streak of
    /// failures so a permanently-broken probe stops making outbound
    /// requests.
    pub(crate) update_probe: Arc<UpdateProbeState>,
    /// Serializes all settings *writes* against each other so the
    /// daemon's sticky onboarding-marker writes (stamped from
    /// [`Self::permission_check`] / [`Self::request_accessibility`])
    /// can't race a frontend `update_settings` IPC and lose the marker.
    /// Reads are still lock-free via `settings_rx`/`store.get_settings`;
    /// the lock only spans the read-modify-write sequence below in
    /// `save_settings` and `mutate_onboarding`.
    settings_write_lock: Arc<AsyncMutex<()>>,
}

impl NagoriRuntime {
    pub fn builder(store: SqliteStore) -> NagoriRuntimeBuilder {
        NagoriRuntimeBuilder {
            store,
            clipboard: None,
            paste: None,
            ai: None,
            permissions: None,
            socket_path: None,
            capabilities: None,
        }
    }

    pub const fn store(&self) -> &SqliteStore {
        &self.store
    }

    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            tx: self.shutdown_tx.clone(),
            rx: self.shutdown_rx.clone(),
        }
    }

    /// Shared handle to the maintenance loop's health snapshot. The
    /// daemon's `serve.rs` calls `record_success` / `record_failure` on
    /// each iteration so the IPC `Health` / `Doctor` handlers can report
    /// degraded retention without round-tripping through the loop.
    pub fn maintenance_health(&self) -> MaintenanceHealth {
        self.maintenance_health.clone()
    }

    /// Shared handle to the capture loop's startup health snapshot.
    /// Whichever process hosts the capture task records `ready` or
    /// `failed(reason)` once initialisation settles; readers (`nagori
    /// doctor`, the desktop's gated notification) see the first
    /// definitive outcome.
    pub fn startup_health(&self) -> StartupHealth {
        self.startup_health.clone()
    }

    /// Shared handle to the capture loop's steady-state health snapshot.
    /// Whichever process hosts the capture task records per-tick outcomes
    /// (success / adapter error / oversized drop / policy refusal /
    /// settings-load error) on this handle; the IPC `Health` and `Doctor`
    /// handlers read it so a silently filtering loop is visible in
    /// `nagori doctor` without grepping logs.
    pub fn capture_health(&self) -> CaptureHealth {
        self.capture_health.clone()
    }

    /// Shared handle to the IPC server's handler-panic counter. The
    /// daemon's `serve.rs` wires this into the accept loops so any
    /// panic surfaced by `JoinSet::join_next()` increments the counter
    /// and updates the most-recent panic message.
    pub fn ipc_health(&self) -> IpcServerHealth {
        self.ipc_health.clone()
    }

    /// Snapshot of the host adapter's capability matrix.
    ///
    /// Returned by clone (a `PlatformCapabilities` is a flat data
    /// struct, not an `Arc`-shared handle) so the IPC dispatcher and
    /// any in-process caller see the same static report regardless of
    /// how the runtime was constructed.
    #[must_use]
    pub fn capabilities(&self) -> PlatformCapabilities {
        (*self.capabilities).clone()
    }

    /// Shared handle to the recent-search cache so out-of-runtime mutators
    /// (notably the [`crate::CaptureLoop`] capture path) can invalidate stale
    /// hits when they push new entries into storage.
    pub fn search_cache_handle(&self) -> SharedSearchCache {
        self.search_cache.clone()
    }

    /// Drop every cached search result and bump the cache epoch.
    ///
    /// Mutation paths must call this both *before* and *after* the storage
    /// write: the pre-call closes the "existing hit served while the
    /// mutation is in flight" window (a concurrent `search` would otherwise
    /// return cached rows that pre-date the mutation between commit and
    /// post-invalidate), while the post-call rejects any stale
    /// [`crate::search_cache::RecentSearchCache::put_if_epoch`] from a
    /// search that started in parallel and snapshotted the older epoch.
    pub fn invalidate_search_cache(&self) {
        lock_or_recover(&self.search_cache).invalidate();
    }

    pub fn settings_subscribe(&self) -> watch::Receiver<AppSettings> {
        self.settings_rx.clone()
    }

    pub fn current_settings(&self) -> AppSettings {
        self.settings_rx.borrow().clone()
    }

    fn publish_settings(&self, settings: AppSettings) {
        // `watch::Sender::send` only fails when *every* receiver has been
        // dropped — i.e. the daemon is mid-teardown or every subscriber
        // (capture loop, maintenance, IPC) has crashed. There is no
        // "stale config" downstream in that case because there is no
        // downstream left, but the absence of subscribers itself is the
        // signal: the daemon's settings fanout has effectively shut down
        // while the runtime keeps accepting writes. Surface it loudly
        // instead of silently swallowing it so this is visible in logs
        // rather than discovered when reload-after-restart "fixes"
        // things.
        if let Err(err) = self.settings_tx.send(settings) {
            error!(error = %err, "settings_broadcast_failed reason=no_receivers");
        }
    }

    pub async fn refresh_settings_from_store(&self) -> Result<AppSettings> {
        let settings = self.store.get_settings().await?;
        self.publish_settings(settings.clone());
        Ok(settings)
    }

    /// Returns the current OS permission status as a list. When no
    /// `PermissionChecker` is wired (e.g. headless tests, non-macOS desktop
    /// builds), returns an empty list rather than erroring so the UI can
    /// still render an "unsupported" hint.
    ///
    /// Side effect: when the checker reports `Accessibility = Granted`
    /// for the first time on this install, the runtime stamps
    /// `settings.onboarding.accessibility_first_granted_at`. The marker
    /// is sticky (a later revoke does not clear it) so the Setup card
    /// can distinguish `RevokedAfterGranted` from a fresh
    /// `PromptShownNotGranted` state.
    pub async fn permission_check(&self) -> Result<Vec<PermissionStatus>> {
        let Some(checker) = self.permissions.clone() else {
            return Ok(Vec::new());
        };
        let current = self.current_settings();
        let ctx = PermissionCheckContext {
            accessibility_prompted_at: current.onboarding.accessibility_prompted_at,
        };
        let statuses = checker.check(&ctx).await?;
        self.stamp_first_grant_if_observed(&statuses).await;
        Ok(statuses)
    }

    /// Idempotently set `onboarding.accessibility_first_granted_at` the
    /// first time we observe `Accessibility = Granted`. Persistence
    /// failures are logged rather than propagated: the marker is
    /// best-effort UX bookkeeping, and the checker's primary contract
    /// is to return the current permission state.
    async fn stamp_first_grant_if_observed(&self, statuses: &[PermissionStatus]) {
        // Cheap guard against re-acquiring the write lock when the
        // marker is already set — the authoritative re-check happens
        // inside `mutate_onboarding`, but skipping the lock entirely on
        // the steady-state hot path keeps the doctor / permission_check
        // poll from serialising behind unrelated settings updates.
        if self
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_some()
        {
            return;
        }
        let observed_grant = statuses.iter().any(|s| {
            s.kind == PermissionKind::Accessibility && s.state == PermissionState::Granted
        });
        if !observed_grant {
            return;
        }
        let result = self
            .mutate_onboarding(|onboarding| {
                // Re-check inside the lock: another writer may have set
                // the marker between the guard above and now.
                if onboarding.accessibility_first_granted_at.is_none() {
                    onboarding.accessibility_first_granted_at = Some(OffsetDateTime::now_utc());
                }
            })
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "onboarding_first_grant_persist_failed");
        }
    }

    /// Trigger the host's accessibility prompt and report the resulting
    /// status. When `prompt = true` the runtime stamps
    /// `onboarding.accessibility_prompted_at` so subsequent
    /// `permission_check` calls discriminate `Denied` from
    /// `NotDetermined`. A `Granted` result also stamps
    /// `accessibility_first_granted_at` (sticky marker).
    pub async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus> {
        let checker = self.permissions.clone().ok_or_else(|| {
            AppError::Unsupported("no permission checker is wired in this runtime".to_owned())
        })?;
        let status = checker.request_accessibility(prompt).await?;
        if prompt {
            // Always refresh the timestamp so dashboards can see "we
            // most recently asked at <t>" rather than the first-ever ask.
            // The UI's NotRequested vs PromptShownNotGranted branch only
            // cares about presence, so overwriting is safe.
            self.mutate_onboarding(|onboarding| {
                onboarding.accessibility_prompted_at = Some(OffsetDateTime::now_utc());
            })
            .await?;
        }
        if status.state == PermissionState::Granted {
            self.stamp_first_grant_if_observed(std::slice::from_ref(&status))
                .await;
        }
        Ok(status)
    }

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

    /// Run a search through the runtime so callers (Tauri, IPC, CLI) all
    /// share the same entry point. Storage-layer access stays on the inside
    /// of this facade so Tauri commands can stay thin.
    ///
    /// Empty queries and short prefix queries are served from
    /// [`crate::search_cache::RecentSearchCache`] when available; longer
    /// queries fall through to `SQLite` directly because the working set
    /// turns over too quickly for caching to help.
    pub async fn search(&self, mut query: SearchQuery) -> Result<Vec<SearchResult>> {
        query.recent_order = self.store.get_settings().await?.recent_order;
        let key = CacheKey::from_query(&query);
        // Capture the epoch we observed at miss time so the post-query `put`
        // can refuse to publish stale results when a concurrent mutation
        // (capture insert, pin toggle, retention sweep, …) called
        // `invalidate` between the SQLite read and our acquisition of the
        // lock again.
        let cached_epoch = if key.is_eligible() {
            let mut cache = lock_or_recover(&self.search_cache);
            match cache.lookup(&key) {
                CacheLookup::Hit(hit) => return Ok(hit),
                CacheLookup::Miss { epoch } => Some(epoch),
            }
        } else {
            None
        };
        let results = self.store.search(query).await?;
        if let Some(epoch) = cached_epoch {
            lock_or_recover(&self.search_cache).put_if_epoch(key, results.clone(), epoch);
        }
        Ok(results)
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

    /// Fetch a cached thumbnail for `id`, or return `None` if the
    /// derived row has not been generated yet.
    ///
    /// Read-only — callers that want lazy generation on miss should
    /// follow this with [`Self::kick_thumbnail_generation`] and either
    /// retry the fetch on the next request (the `nagori-image://thumb/`
    /// path's `503 Retry-After`) or stream the original payload.
    pub async fn get_thumbnail(&self, id: EntryId) -> Result<Option<ThumbnailRecord>> {
        self.store.get_thumbnail(id).await
    }

    /// Kick a background thumbnail generation for `id` if one is not
    /// already in flight, returning immediately.
    ///
    /// The generator is gated by [`ThumbnailGate`] so concurrent requests
    /// for the same entry collapse to a single decoder, and re-asserts
    /// the sensitivity check inside [`thumbnails::generate_thumbnail`]
    /// as a best-effort application-layer guard — that re-read narrows
    /// the TOCTOU window so a caller bypassing the dispatch gate with a
    /// stale classification typically loses the race before
    /// `put_thumbnail` runs. The window is not closed at this layer;
    /// see `generate_thumbnail` for the storage-side invariant that
    /// would be required for a hard guarantee.
    /// Once generation completes, [`SqliteStore::enforce_thumbnail_budget`]
    /// is invoked to apply the LRU sweep if the operator configured one.
    pub fn kick_thumbnail_generation(&self, id: EntryId) {
        let Some(guard) = self.thumbnail_gate.try_acquire(id) else {
            // Another request is already generating this thumbnail; the
            // first caller's `put_thumbnail` will satisfy us on the next
            // fetch.
            return;
        };
        let store = self.store.clone();
        let settings_rx = self.settings_rx.clone();
        let gate = self.thumbnail_gate.clone();
        tokio::spawn(async move {
            // Hold the gate guard across the whole generation so a
            // second request that beats us to the cache lookup still
            // observes the in-flight slot.
            let _guard = guard;
            // Bound the global decode concurrency before pulling bytes
            // off disk or touching the blocking pool. Per-entry dedupe
            // (the gate guard above) does nothing for misses that span
            // distinct entries — a prefetch sweep or image-heavy scroll
            // would otherwise pile up `tokio::spawn` tasks each ready to
            // allocate hundreds of MiB for the decode buffer.
            let _permit = match gate.acquire_permit().await {
                Ok(permit) => permit,
                Err(err) => {
                    tracing::warn!(error = %err, entry_id = %id, "thumbnail_permit_unavailable");
                    return;
                }
            };
            match thumbnails::generate_thumbnail(&store, id).await {
                Ok(Some(_)) => {
                    let budget = settings_rx.borrow().max_thumbnail_total_bytes;
                    if let Some(budget) = budget
                        && let Err(err) = store.enforce_thumbnail_budget(budget).await
                    {
                        tracing::warn!(error = %err, "thumbnail_budget_enforce_failed");
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(error = %err, entry_id = %id, "thumbnail_generate_failed");
                }
            }
        });
    }

    pub async fn get_settings(&self) -> Result<AppSettings> {
        self.store.get_settings().await
    }

    /// Persist updated settings *and* re-publish them on the watch channel
    /// so the capture loop and other subscribers pick up the change without
    /// the caller having to remember the second step.
    ///
    /// The runtime owns the `onboarding` markers: any value the caller
    /// passes in that field is silently replaced with the currently
    /// persisted state inside the write lock, so an `update_settings`
    /// from the desktop shell can never wipe an `accessibility_*` marker
    /// it didn't know about. Marker writes themselves go through
    /// [`Self::mutate_onboarding`], which acquires the same lock.
    pub async fn save_settings(&self, settings: AppSettings) -> Result<()> {
        let _guard = self.settings_write_lock.lock().await;
        let persisted = self.store.get_settings().await?;
        let mut merged = settings;
        merged.onboarding = persisted.onboarding;
        self.store.save_settings(merged.clone()).await?;
        self.publish_settings(merged);
        Ok(())
    }

    /// Apply `f` to the `onboarding` namespace under the settings write
    /// lock, reading the latest persisted state inside the critical
    /// section so a concurrent [`Self::save_settings`] cannot lose the
    /// marker. The other settings fields are left untouched.
    async fn mutate_onboarding<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut OnboardingSettings),
    {
        let _guard = self.settings_write_lock.lock().await;
        let mut settings = self.store.get_settings().await?;
        f(&mut settings.onboarding);
        self.store.save_settings(settings.clone()).await?;
        self.publish_settings(settings);
        Ok(())
    }

    /// Toggle `capture_enabled` without round-tripping the entire settings
    /// blob — used by the tray menu and the `set_capture_enabled` Tauri
    /// command. Returns the post-update settings.
    pub async fn set_capture_enabled(&self, enabled: bool) -> Result<AppSettings> {
        let mut settings = self.store.get_settings().await?;
        if settings.capture_enabled != enabled {
            settings.capture_enabled = enabled;
            self.save_settings(settings.clone()).await?;
        }
        Ok(settings)
    }

    pub async fn run_ai_action(
        &self,
        id: EntryId,
        action: AiActionId,
    ) -> Result<nagori_core::AiOutput> {
        let settings = self.store.get_settings().await?;
        let action_def = self
            .ai_registry
            .get(action)
            .ok_or_else(|| AppError::InvalidInput(format!("unknown ai action {action:?}")))?;
        let policy = action_def.input_policy.clone();
        // Quick actions (Summarize / FormatJson / ExtractTasks /
        // RedactSecrets) always run on-device against the rule-based
        // runner, regardless of the legacy `ai_enabled` / `ai_provider`
        // settings — those toggles no longer have a UI entry point but
        // remain in `AppSettings` as schema-compat seats so a future
        // remote provider can be reintroduced without a settings
        // migration. The input policy (redaction, size cap) below
        // still applies. Legacy LLM-only variants keep the original
        // gating so a direct IPC caller can't accidentally bypass the
        // user's provider choice.
        if !action.is_quick_action() {
            if !settings.ai_enabled {
                return Err(AppError::Policy(
                    "ai actions are disabled in settings".to_owned(),
                ));
            }
            // Provider gating: `None` blocks everything; `Local` runs the
            // on-device runner; `Remote` is reserved as an extension
            // point for a future OpenAI-compatible generic provider but
            // has no implementation wired in this build, so we surface
            // an Unsupported error rather than silently fall through to
            // the local runner. The `allow_remote` per-action policy is
            // kept on `AiActionDef` so the gate site stays one line when
            // a remote provider is later wired back in.
            match &settings.ai_provider {
                AiProviderSetting::None => {
                    return Err(AppError::Policy(
                        "ai_provider is set to None — refusing to run".to_owned(),
                    ));
                }
                AiProviderSetting::Local => {}
                AiProviderSetting::Remote { .. } => {
                    return Err(AppError::Unsupported(
                        "remote ai_provider has no implementation wired in this build".to_owned(),
                    ));
                }
            }
        }
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        let raw = entry.plain_text().unwrap_or_default();
        // The redactor here must be settings-aware: a bare `redact_text`
        // applies only the built-in patterns and silently leaks anything
        // matched by `regex_denylist`. Constructing the classifier from
        // the same `settings` we just loaded ensures user-supplied rules
        // gate the AI input the same way they gate the preview pane.
        let classifier = SensitivityClassifier::try_new(settings.clone())?;
        // Input shaping: secrets must be redacted (or refused), private
        // entries are redacted unconditionally, and `require_redaction`
        // forces redaction even on Public entries before the provider sees
        // the text.
        let input = match (entry.sensitivity, action) {
            (Sensitivity::Secret | Sensitivity::Blocked, AiActionId::RedactSecrets) => {
                classifier.redact(raw)
            }
            (Sensitivity::Secret | Sensitivity::Blocked, _) => {
                return Err(AppError::Policy(
                    "secret entries must be redacted before this AI action".to_owned(),
                ));
            }
            (Sensitivity::Private, _) => classifier.redact(raw),
            _ => {
                if policy.require_redaction {
                    classifier.redact(raw)
                } else {
                    raw.to_owned()
                }
            }
        };
        // Size cap: refuse rather than silently truncating — truncation can
        // produce surprising AI output (cut-off code, half-translated text)
        // and the user can re-issue with a smaller selection if they want.
        if input.len() > policy.max_bytes {
            return Err(AppError::Policy(format!(
                "input exceeds max_bytes ({}) for action {}",
                policy.max_bytes, action_def.name
            )));
        }
        self.ai.run_action(action, &input).await
    }
}

pub struct NagoriRuntimeBuilder {
    store: SqliteStore,
    clipboard: Option<Arc<dyn ClipboardWriter>>,
    paste: Option<Arc<dyn PasteController>>,
    ai: Option<Arc<dyn AiProvider>>,
    permissions: Option<Arc<dyn PermissionChecker>>,
    socket_path: Option<std::path::PathBuf>,
    capabilities: Option<PlatformCapabilities>,
}

impl NagoriRuntimeBuilder {
    #[must_use]
    pub fn clipboard(mut self, clipboard: Arc<dyn ClipboardWriter>) -> Self {
        self.clipboard = Some(clipboard);
        self
    }

    #[must_use]
    pub fn paste(mut self, paste: Arc<dyn PasteController>) -> Self {
        self.paste = Some(paste);
        self
    }

    #[must_use]
    pub fn ai(mut self, ai: Arc<dyn AiProvider>) -> Self {
        self.ai = Some(ai);
        self
    }

    #[must_use]
    pub fn permissions(mut self, permissions: Arc<dyn PermissionChecker>) -> Self {
        self.permissions = Some(permissions);
        self
    }

    #[must_use]
    pub fn socket_path(mut self, path: std::path::PathBuf) -> Self {
        self.socket_path = Some(path);
        self
    }

    /// Set the host adapter's capability report.
    ///
    /// `nagori-platform-native::build_native_runtime` populates this
    /// with `nagori_platform_native::capabilities()` so the runtime
    /// and the IPC `Capabilities` handler return the same static
    /// matrix. Daemon-internal tests fall back to
    /// `nagori_platform::unsupported_capabilities()`.
    #[must_use]
    pub fn capabilities(mut self, capabilities: PlatformCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Build a production runtime.
    ///
    /// Requires `clipboard` and `paste` adapters — those are platform
    /// integrations whose absence would make the app silently inert
    /// (capture never fires, `paste_frontmost` always no-ops). Missing
    /// either returns `AppError::Configuration` so wiring drift surfaces
    /// at startup instead of as mysterious runtime behaviour.
    ///
    /// `ai`, `permissions`, and `socket_path` remain optional: AI falls
    /// back to a mock provider, permissions are genuinely platform-
    /// optional, and an empty socket path is meaningful for daemons that
    /// only serve in-process callers.
    ///
    /// Tests that need a runtime without real adapters should call
    /// [`Self::build_for_test`].
    pub fn build(mut self) -> std::result::Result<NagoriRuntime, AppError> {
        let clipboard = self.clipboard.take().ok_or_else(|| {
            AppError::Configuration(
                "clipboard adapter is required in production runtime".to_owned(),
            )
        })?;
        let paste = self.paste.take().ok_or_else(|| {
            AppError::Configuration("paste controller is required in production runtime".to_owned())
        })?;
        Ok(self.assemble(clipboard, paste))
    }

    /// Build a runtime suitable for tests, supplying dummy adapters
    /// (`MemoryClipboard`, `NoopPasteController`, `MockAiProvider`)
    /// for anything the caller did not set explicitly.
    ///
    /// Production code must use [`Self::build`] so that adapter wiring
    /// gaps surface as `AppError::Configuration` instead of silently
    /// substituting in-memory stubs.
    #[must_use]
    pub fn build_for_test(mut self) -> NagoriRuntime {
        let clipboard = self
            .clipboard
            .take()
            .unwrap_or_else(|| Arc::new(MemoryClipboard::new()));
        let paste = self
            .paste
            .take()
            .unwrap_or_else(|| Arc::new(NoopPasteController));
        self.assemble(clipboard, paste)
    }

    fn assemble(
        self,
        clipboard: Arc<dyn ClipboardWriter>,
        paste: Arc<dyn PasteController>,
    ) -> NagoriRuntime {
        let (settings_tx, settings_rx) = watch::channel(AppSettings::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // Headless callers (the CLI's `add` / `ai` paths, in-process
        // tests) never expose IPC, so the capability report is never
        // queried — default to `unsupported_capabilities()` rather than
        // forcing those sites to wire a value they don't need.
        // Production paths flow through `nagori-platform-native::
        // build_native_runtime`, which sets the host's real report.
        let capabilities = Arc::new(self.capabilities.unwrap_or_else(unsupported_capabilities));
        NagoriRuntime {
            store: self.store,
            clipboard,
            paste,
            ai: self.ai.unwrap_or_else(|| Arc::new(MockAiProvider)),
            ai_registry: Arc::new(AiActionRegistry::default()),
            permissions: self.permissions,
            shutdown_tx,
            shutdown_rx,
            settings_tx,
            settings_rx,
            socket_path: Arc::new(self.socket_path.unwrap_or_default()),
            search_cache: new_shared_cache(),
            maintenance_health: MaintenanceHealth::new(),
            startup_health: StartupHealth::new(),
            capture_health: CaptureHealth::new(),
            ipc_health: IpcServerHealth::new(),
            capabilities,
            thumbnail_gate: ThumbnailGate::default(),
            update_probe: Arc::new(UpdateProbeState::default()),
            settings_write_lock: Arc::new(AsyncMutex::new(())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl ShutdownHandle {
    pub fn cancel(&self) {
        let _ = self.tx.send_replace(true);
    }

    pub async fn cancelled(&mut self) {
        if *self.rx.borrow_and_update() {
            return;
        }
        loop {
            if self.rx.changed().await.is_err() {
                return;
            }
            if *self.rx.borrow_and_update() {
                return;
            }
        }
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
        Err(AppError::Platform(result.message.unwrap_or_else(|| {
            "auto-paste did not run; OS paste controller reported pasted=false".to_owned()
        })))
    }
}

/// Minimum interval between two successful or failed GitHub probes. A
/// new release lands at most every few days; a 24h floor keeps the
/// daemon from hammering `api.github.com` when an operator scripts
/// `nagori doctor` in a loop (or when a network flap fails every
/// request within the rate-limit window).
const UPDATE_PROBE_MIN_INTERVAL: Duration = Duration::from_hours(24);

/// Consecutive failure count after which the probe hard-disables for the
/// remainder of the daemon's lifetime. Five strikes covers the typical
/// transient-failure window (DNS flap, captive portal) without leaving
/// the probe running forever against a permanently-broken environment.
const UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Caches the latest `fetch_latest_release_version` outcome and gates
/// re-attempts behind a 24h floor + hard-disable on repeated failure.
///
/// The state lives on `NagoriRuntime` (wrapped in `Arc` to keep `Clone`
/// cheap) so every IPC `Doctor` call shares the same cache — without
/// this the previous implementation made an HTTP request on every
/// doctor invocation, which is fine for an interactive operator but
/// pathological for monitoring jobs that poll the endpoint.
pub(crate) struct UpdateProbeState {
    inner: Mutex<UpdateProbeInner>,
}

impl Default for UpdateProbeState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(UpdateProbeInner::default()),
        }
    }
}

#[derive(Default)]
struct UpdateProbeInner {
    /// Last time we *attempted* the probe (success or failure). `None`
    /// means we have not probed since the daemon started.
    last_attempt: Option<Instant>,
    /// Cached tag from the most recent successful probe. Stays valid
    /// until the next successful probe overwrites it; failures do not
    /// invalidate the cache so a flake doesn't downgrade doctor from
    /// "you're behind" to "(unknown)" on the next call.
    cached_version: Option<String>,
    /// Count of consecutive probe failures since the last success.
    /// Reset to zero on every successful probe.
    consecutive_failures: u32,
    /// Once `consecutive_failures` crosses
    /// [`UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES`] we stop probing for
    /// the rest of the daemon's lifetime. Cleared on a restart, which
    /// is the appropriate recovery boundary — a daemon that keeps
    /// failing for hours is not going to recover within the same
    /// process.
    hard_disabled: bool,
}

impl UpdateProbeState {
    /// Return the cached tag if a fresh probe is not due, or perform a
    /// probe and cache the result. Always `Some(_)` once a successful
    /// probe has landed; `None` while uninitialised, during a probe
    /// failure, or after the hard-disable threshold is crossed.
    pub(crate) async fn fetch_if_due(&self) -> Option<String> {
        // Reserve the probe slot under the lock before dropping it: a
        // bare snapshot would let several concurrent doctor IPCs all
        // observe a stale `last_attempt` and burst-call GitHub in
        // parallel, defeating the 24h rate limit and stacking
        // `consecutive_failures` per call rather than per window. By
        // bumping `last_attempt` *before* the HTTP await, parallel
        // callers see a recent attempt and return the cached value
        // (possibly slightly stale) instead of starting their own
        // probe; the lock is still released across the network call
        // so a slow probe never blocks an unrelated doctor caller.
        let now = Instant::now();
        {
            let mut inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if inner.hard_disabled {
                return inner.cached_version.clone();
            }
            if let Some(last) = inner.last_attempt
                && now.duration_since(last) < UPDATE_PROBE_MIN_INTERVAL
            {
                return inner.cached_version.clone();
            }
            inner.last_attempt = Some(now);
        }

        let result = fetch_latest_release_version().await;
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(version) = result {
            inner.cached_version = Some(version);
            inner.consecutive_failures = 0;
        } else {
            inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
            if inner.consecutive_failures >= UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES {
                inner.hard_disabled = true;
                tracing::warn!(
                    consecutive_failures = inner.consecutive_failures,
                    "update_probe_hard_disabled",
                );
            }
        }
        inner.cached_version.clone()
    }
}

/// Best-effort lookup of the latest released `nagori` tag on GitHub.
///
/// The doctor handler calls this through [`UpdateProbeState::fetch_if_due`]
/// so the bare function only handles the network round-trip; gating and
/// caching live one level up. Strict timeout, no retries: if GitHub is
/// unreachable, rate-limiting us, or returns an unexpected payload, we
/// return `None` and doctor renders "(unknown)" rather than failing the
/// whole report.
async fn fetch_latest_release_version() -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("nagori/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let release: Release = client
        .get("https://api.github.com/repos/mhiro2/nagori/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some(release.tag_name.trim_start_matches('v').to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use nagori_core::{EntryRepository, SettingsRepository};
    use nagori_ipc::{
        AddEntryRequest, EntryDto, GetEntryRequest, IpcRequest, IpcResponse, ListPinnedRequest,
        SearchRequest, SearchResponse, UpdateSettingsRequest,
    };
    use nagori_platform::{MemoryClipboard, PasteResult};

    use super::*;

    fn runtime_with_memory_clipboard() -> (NagoriRuntime, Arc<MemoryClipboard>) {
        let store = SqliteStore::open_memory().expect("memory store should open");
        let clipboard = Arc::new(MemoryClipboard::new());
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .build_for_test();
        (runtime, clipboard)
    }

    fn runtime_with_local_ai() -> (NagoriRuntime, Arc<MemoryClipboard>) {
        // The default test builder wires `MockAiProvider`, which echoes
        // the action name + input back as a single string. That's fine
        // for gate-level assertions, but Quick-action contracts also
        // need to prove the real on-device runner accepts the inputs —
        // wire `LocalAiProvider` explicitly for those cases.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let clipboard = Arc::new(MemoryClipboard::new());
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .ai(Arc::new(nagori_ai::LocalAiProvider::default()))
            .build_for_test();
        (runtime, clipboard)
    }

    #[derive(Default)]
    struct CountingPaste {
        calls: AtomicUsize,
    }

    impl CountingPaste {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl PasteController for CountingPaste {
        async fn paste_frontmost(&self) -> Result<PasteResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(PasteResult {
                pasted: true,
                message: None,
            })
        }
    }

    fn runtime_with_paste(
        paste: Arc<dyn PasteController>,
    ) -> (NagoriRuntime, Arc<MemoryClipboard>) {
        let store = SqliteStore::open_memory().expect("memory store should open");
        let clipboard = Arc::new(MemoryClipboard::new());
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .paste(paste)
            .build_for_test();
        (runtime, clipboard)
    }

    #[tokio::test]
    async fn doctor_report_reflects_startup_health_outcome() {
        // Lock the wiring from `StartupHealth` into the `Doctor` IPC
        // handler: `nagori doctor` is the operator-facing surface where
        // a silent capture-init abort has to be visible. Without this
        // test, dropping the `startup` field from `DoctorReport` (or
        // forgetting to record it) would compile cleanly and re-introduce
        // the original "looks ready, isn't" bug.
        let (runtime, _) = runtime_with_memory_clipboard();
        let pending = runtime
            .build_doctor_report()
            .await
            .expect("doctor report builds with default startup state");
        assert!(
            !pending.startup.ready,
            "default startup state must report not-ready"
        );
        assert!(pending.startup.last_error.is_none());

        runtime
            .startup_health()
            .record_capture_failed("could not load settings");
        let failed = runtime
            .build_doctor_report()
            .await
            .expect("doctor report builds after recording a failure");
        assert!(!failed.startup.ready);
        assert_eq!(
            failed.startup.last_error.as_deref(),
            Some("could not load settings"),
        );

        // Late `record_capture_ready` must not flip a recorded failure
        // back to ready — `StartupHealth` is first-outcome-wins.
        runtime.startup_health().record_capture_ready();
        let still_failed = runtime
            .build_doctor_report()
            .await
            .expect("doctor report builds after a no-op ready record");
        assert!(!still_failed.startup.ready);
        assert!(still_failed.startup.last_error.is_some());
    }

    #[tokio::test]
    async fn doctor_report_marks_ready_once_capture_records_success() {
        // Positive case: once the host process records readiness, the
        // doctor surface reports it without needing any additional
        // wiring. Pair with the failure test above so a future refactor
        // that hard-codes `ready: false` or `ready: true` in the
        // builder is caught.
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime.startup_health().record_capture_ready();
        let report = runtime
            .build_doctor_report()
            .await
            .expect("doctor report builds after recording readiness");
        assert!(report.startup.ready);
        assert!(report.startup.last_error.is_none());
    }

    #[tokio::test]
    async fn shutdown_ipc_is_observed_after_worker_starts_waiting() {
        let (runtime, _) = runtime_with_memory_clipboard();
        let mut shutdown = runtime.shutdown_handle();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            release_rx.await.expect("worker release should be sent");
            shutdown.cancelled().await;
        });

        let response = runtime.handle_ipc(IpcRequest::Shutdown).await;
        assert!(matches!(response, IpcResponse::Ack));

        release_tx.send(()).expect("worker should still be alive");
        tokio::time::timeout(std::time::Duration::from_millis(100), worker)
            .await
            .expect("shutdown should remain visible after the IPC request")
            .expect("worker should not panic");
    }

    #[tokio::test]
    async fn add_entry_ipc_persists_and_searches_text() {
        let (runtime, _) = runtime_with_memory_clipboard();

        let response = runtime
            .handle_ipc(IpcRequest::AddEntry(AddEntryRequest {
                text: "Clipboard history value".to_owned(),
            }))
            .await;
        let IpcResponse::Entry(EntryDto { id, text, .. }) = response else {
            panic!("expected entry response");
        };

        assert_eq!(text.as_deref(), Some("Clipboard history value"));

        let response = runtime
            .handle_ipc(IpcRequest::Search(SearchRequest {
                query: "history".to_owned(),
                limit: 10,
            }))
            .await;
        let IpcResponse::Search(SearchResponse { results }) = response else {
            panic!("expected search response");
        };

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
    }

    #[tokio::test]
    async fn paste_entry_skips_keystroke_when_auto_paste_disabled() {
        let paste = Arc::new(CountingPaste::default());
        let (runtime, clipboard) = runtime_with_paste(paste.clone());
        runtime
            .store()
            .save_settings(AppSettings {
                auto_paste_enabled: false,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        let id = runtime
            .add_text("paste me".to_owned())
            .await
            .expect("entry should be added");

        runtime
            .paste_entry(id, None)
            .await
            .expect("paste should succeed");

        assert_eq!(clipboard.current_text().as_deref(), Some("paste me"));
        assert_eq!(paste.calls(), 0, "auto-paste must not fire by default");
    }

    #[tokio::test]
    async fn paste_entry_pastes_when_auto_paste_enabled() {
        let paste = Arc::new(CountingPaste::default());
        let (runtime, _) = runtime_with_paste(paste.clone());
        runtime
            .store()
            .save_settings(AppSettings {
                auto_paste_enabled: true,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");

        let id = runtime
            .add_text("paste me".to_owned())
            .await
            .expect("entry should be added");
        runtime
            .paste_entry(id, None)
            .await
            .expect("paste should succeed");

        assert_eq!(paste.calls(), 1);
    }

    #[tokio::test]
    async fn copy_entry_writes_clipboard_and_increments_use_count() {
        let (runtime, clipboard) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("copy me".to_owned())
            .await
            .expect("entry should be added");

        runtime.copy_entry(id).await.expect("copy should succeed");

        assert_eq!(clipboard.current_text().as_deref(), Some("copy me"));
        let entry = runtime
            .store()
            .get(id)
            .await
            .expect("store read should succeed")
            .expect("entry should exist");
        assert_eq!(entry.metadata.use_count, 1);
        assert!(entry.metadata.last_used_at.is_some());
    }

    #[tokio::test]
    async fn copy_entry_preserve_hydrates_stored_representations() {
        // Entries captured via the snapshot path persist every preserved
        // representation. Preserve copy-back must replay the whole set
        // through `write_representations` so a multi-rep-aware adapter can
        // re-offer the same MIME variants the source advertised. Use a
        // recording writer to lock the dispatch order: empty rep set →
        // `write_entry`; populated set → `write_representations`.
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash, EntryFactory, RepresentationRole, StoredClipboardRepresentation,
        };
        use time::OffsetDateTime;

        #[derive(Default)]
        struct RecordingWriter {
            entry_calls: tokio::sync::Mutex<Vec<EntryId>>,
            rep_calls: tokio::sync::Mutex<Vec<(EntryId, Vec<StoredClipboardRepresentation>)>>,
        }

        #[async_trait]
        impl ClipboardWriter for RecordingWriter {
            async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
                self.entry_calls.lock().await.push(entry.id);
                Ok(())
            }

            async fn write_plain(&self, _entry: &ClipboardEntry) -> Result<()> {
                Ok(())
            }

            async fn write_text(&self, _text: &str) -> Result<()> {
                Ok(())
            }

            async fn write_representations(
                &self,
                entry: &ClipboardEntry,
                representations: &[StoredClipboardRepresentation],
            ) -> Result<()> {
                self.rep_calls
                    .lock()
                    .await
                    .push((entry.id, representations.to_vec()));
                Ok(())
            }
        }

        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash(
                ContentHash::sha256(b"preserve-hydration").value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text(
                        "<p>preserve hydration <strong>html</strong></p>".to_owned(),
                    ),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("preserve hydration plain".to_owned()),
                },
            ],
        };
        let entry = EntryFactory::from_snapshot(snapshot)
            .expect("snapshot should yield an entry with stored representations");
        assert!(
            !entry.pending_representations.is_empty(),
            "fixture must produce a multi-rep entry",
        );

        let writer = Arc::new(RecordingWriter::default());
        let store = SqliteStore::open_memory().expect("memory store should open");
        let runtime = NagoriRuntime::builder(store)
            .clipboard(writer.clone() as Arc<dyn ClipboardWriter>)
            .build_for_test();
        let id = runtime
            .store()
            .insert(entry)
            .await
            .expect("insert snapshot-derived entry");

        runtime.copy_entry(id).await.expect("preserve copy");

        let entry_calls = writer.entry_calls.lock().await.clone();
        let rep_calls = writer.rep_calls.lock().await.clone();
        assert!(
            entry_calls.is_empty(),
            "Preserve must route through write_representations, not write_entry; saw {entry_calls:?}",
        );
        assert_eq!(rep_calls.len(), 1, "expected exactly one rep-set write");
        let (called_id, reps) = &rep_calls[0];
        assert_eq!(*called_id, id);
        assert!(
            reps.iter()
                .any(|rep| rep.role == RepresentationRole::Primary && rep.mime_type == "text/html"),
            "stored rep set must include the HTML primary, got {reps:?}",
        );
        assert!(
            reps.iter()
                .any(|rep| rep.role == RepresentationRole::PlainFallback
                    && rep.mime_type == "text/plain"),
            "stored rep set must include the plain fallback, got {reps:?}",
        );
    }

    #[tokio::test]
    async fn sensitive_entries_hide_text_until_sensitive_output_is_requested() {
        // OTP-shaped clips classify as Secret and get persisted as
        // `[REDACTED]` under the default `StoreRedacted`. The IPC gate
        // still applies on top of that: without `include_sensitive` the
        // body is suppressed entirely; with it the caller sees the
        // redacted form (the raw OTP never reached SQLite, so there is
        // nothing else to reveal).
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("123456".to_owned())
            .await
            .expect("OTP should be stored as redacted Secret");

        let hidden = runtime
            .handle_ipc(IpcRequest::GetEntry(GetEntryRequest {
                id,
                include_sensitive: false,
            }))
            .await;
        let IpcResponse::Entry(hidden) = hidden else {
            panic!("expected hidden entry");
        };
        assert!(hidden.text.is_none());

        let visible = runtime
            .handle_ipc(IpcRequest::GetEntry(GetEntryRequest {
                id,
                include_sensitive: true,
            }))
            .await;
        let IpcResponse::Entry(visible) = visible else {
            panic!("expected visible entry");
        };
        assert_eq!(visible.text.as_deref(), Some("[REDACTED]"));
    }

    #[tokio::test]
    async fn list_pinned_honours_include_sensitive_flag() {
        // Pinned entries previously came back with `text: None` regardless
        // of sensitivity, so even Public pins lost their body and any
        // sensitive pin couldn't be opted-in to. Now the response mirrors
        // ListRecent: Public bodies are always emitted; sensitive bodies
        // require `include_sensitive: true`. The OTP body is redacted on
        // insert (StoreRedacted), so the include_sensitive=true response
        // surfaces `[REDACTED]` rather than the raw 6-digit code.
        let (runtime, _) = runtime_with_memory_clipboard();
        let public_id = runtime
            .add_text("public clipboard text".to_owned())
            .await
            .expect("public entry");
        let secret_id = runtime
            .add_text("123456".to_owned())
            .await
            .expect("OTP entry");
        runtime
            .store()
            .set_pinned(public_id, true)
            .await
            .expect("pin public");
        runtime
            .store()
            .set_pinned(secret_id, true)
            .await
            .expect("pin secret");

        let hidden = runtime
            .handle_ipc(IpcRequest::ListPinned(ListPinnedRequest {
                include_sensitive: false,
            }))
            .await;
        let IpcResponse::Entries(hidden) = hidden else {
            panic!("expected entries response, got {hidden:?}");
        };
        let public = hidden.iter().find(|dto| dto.id == public_id).unwrap();
        let secret = hidden.iter().find(|dto| dto.id == secret_id).unwrap();
        assert_eq!(
            public.text.as_deref(),
            Some("public clipboard text"),
            "public pinned entry must retain body without opt-in",
        );
        assert!(
            secret.text.is_none(),
            "sensitive pinned entry must hide body without opt-in",
        );

        let visible = runtime
            .handle_ipc(IpcRequest::ListPinned(ListPinnedRequest {
                include_sensitive: true,
            }))
            .await;
        let IpcResponse::Entries(visible) = visible else {
            panic!("expected entries response");
        };
        let secret = visible.iter().find(|dto| dto.id == secret_id).unwrap();
        assert_eq!(secret.text.as_deref(), Some("[REDACTED]"));
    }

    #[tokio::test]
    async fn update_settings_ipc_persists_and_publishes_current_settings() {
        let (runtime, _) = runtime_with_memory_clipboard();
        let settings = AppSettings {
            capture_enabled: false,
            global_hotkey: "CmdOrCtrl+Alt+V".to_owned(),
            ..Default::default()
        };
        let value = serde_json::to_value(&settings).expect("settings should serialize");

        let response = runtime
            .handle_ipc(IpcRequest::UpdateSettings(UpdateSettingsRequest { value }))
            .await;

        assert!(matches!(response, IpcResponse::Ack));
        assert_eq!(runtime.current_settings().global_hotkey, "CmdOrCtrl+Alt+V");
        assert!(!runtime.current_settings().capture_enabled);
        let persisted = runtime
            .store()
            .get_settings()
            .await
            .expect("settings should persist");
        assert_eq!(persisted, settings);
    }

    #[tokio::test]
    async fn disabled_cli_ipc_rejects_non_control_requests() {
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime
            .save_settings(AppSettings {
                cli_ipc_enabled: false,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");

        let rejected = runtime
            .handle_ipc(IpcRequest::AddEntry(AddEntryRequest {
                text: "blocked".to_owned(),
            }))
            .await;
        let IpcResponse::Error(err) = rejected else {
            panic!("expected disabled IPC to reject writes");
        };
        assert_eq!(err.code, "permission_error");

        let health = runtime.handle_ipc(IpcRequest::Health).await;
        assert!(
            matches!(health, IpcResponse::Health(_)),
            "health must remain available while IPC is disabled",
        );

        // Capabilities is read-only and treated as a control request,
        // so it must also bypass the cli_ipc_enabled gate. Otherwise
        // a user disabling CLI IPC would also blind the doctor / UI
        // to the OS capability matrix.
        let capabilities = runtime.handle_ipc(IpcRequest::Capabilities).await;
        assert!(
            matches!(capabilities, IpcResponse::Capabilities(_)),
            "capabilities must remain available while IPC is disabled",
        );
    }

    #[tokio::test]
    async fn capabilities_handler_returns_builder_value() {
        // Builder-supplied capabilities must round-trip through the
        // dispatcher — that's the contract the desktop + CLI rely on,
        // so they can render exactly what the daemon was started with
        // rather than reprobing the OS in two places.
        use nagori_platform::{Capability, Platform, SupportTier};

        let store = SqliteStore::open_memory().expect("memory store should open");
        let expected = PlatformCapabilities {
            platform: Platform::MacOS,
            tier: SupportTier::Supported,
            capture_text: Capability::Available,
            capture_image: Capability::Available,
            capture_files: Capability::Available,
            write_text: Capability::Available,
            write_image: Capability::Available,
            clipboard_multi_representation_write: Capability::Available,
            auto_paste: Capability::Available,
            global_hotkey: Capability::Available,
            frontmost_app: Capability::Available,
            permissions_ui: Capability::Available,
            update_check: Capability::Available,
            preview_quick_look: Capability::Available,
        };
        let runtime = NagoriRuntime::builder(store)
            .clipboard(Arc::new(MemoryClipboard::new()))
            .capabilities(expected.clone())
            .build_for_test();

        let response = runtime.handle_ipc(IpcRequest::Capabilities).await;
        let IpcResponse::Capabilities(actual) = response else {
            panic!("expected Capabilities response");
        };
        assert_eq!(*actual, expected);
    }

    #[tokio::test]
    async fn run_ai_action_blocked_when_ai_disabled_for_legacy_action() {
        // Legacy LLM-only variants still respect the `ai_enabled` master
        // switch so IPC callers can't bypass a deliberate "no AI" config
        // by reaching for `Translate` / `Rewrite` etc. — only the four
        // Quick actions are exempt because they have no UI toggle left.
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("hello".to_owned())
            .await
            .expect("entry should be added");
        let err = runtime
            .run_ai_action(id, AiActionId::Rewrite)
            .await
            .expect_err("legacy ai actions must be refused when ai_enabled=false");
        assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_ai_action_quick_action_bypasses_default_gates() {
        // Defaults are `ai_enabled=false`, `ai_provider=None`. The
        // desktop UI no longer surfaces any of these toggles, so the
        // four Quick actions must still execute against the on-device
        // runner under the default config or the palette's "Summarize /
        // Format JSON / Extract tasks / Redact secrets" buttons would
        // be perma-broken. Each action gets a stable input that the
        // real `LocalAiProvider` accepts — `FormatJson` in particular
        // needs valid JSON, since anything else would surface as
        // `AppError::Ai` and look like the gate is still rejecting.
        let cases: &[(AiActionId, &str)] = &[
            (AiActionId::Summarize, "hello world"),
            (AiActionId::FormatJson, r#"{"a":1}"#),
            (AiActionId::ExtractTasks, "TODO: ship the thing"),
            (AiActionId::RedactSecrets, "no secrets here"),
        ];
        for (action, input) in cases {
            let (runtime, _) = runtime_with_local_ai();
            let id = runtime
                .add_text((*input).to_owned())
                .await
                .expect("entry should be added");
            runtime
                .run_ai_action(id, *action)
                .await
                .unwrap_or_else(|err| panic!("{action:?} must run under defaults; got {err:?}"));
        }
    }

    #[tokio::test]
    async fn run_ai_action_quick_action_ignores_remote_provider_config() {
        // A persisted `ai_provider = Remote { .. }` from an older config
        // must not change Quick-action behaviour: they always run
        // on-device, regardless of `allow_remote`.
        let (runtime, _) = runtime_with_local_ai();
        runtime
            .save_settings(AppSettings {
                ai_enabled: true,
                ai_provider: AiProviderSetting::Remote {
                    name: "openai".to_owned(),
                },
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        let id = runtime
            .add_text("note".to_owned())
            .await
            .expect("entry should be added");
        runtime
            .run_ai_action(id, AiActionId::Summarize)
            .await
            .expect("quick action must succeed even with Remote provider stored");
    }

    #[tokio::test]
    async fn run_ai_action_remote_provider_returns_unsupported_for_legacy_actions() {
        // No remote provider is wired in this build. Legacy AI actions
        // (driven through `Rewrite` here, since Quick actions
        // short-circuit) must surface that as `AppError::Unsupported`
        // rather than silently fall through to the on-device runner —
        // the latter would mask "you asked for a remote summary, here's
        // a local one" from a direct IPC caller.
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime
            .store()
            .save_settings(AppSettings {
                ai_enabled: true,
                ai_provider: AiProviderSetting::Remote {
                    name: "openai".to_owned(),
                },
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        let id = runtime
            .add_text("hello".to_owned())
            .await
            .expect("entry should be added");
        let err = runtime
            .run_ai_action(id, AiActionId::Rewrite)
            .await
            .expect_err("remote ai_provider must surface as Unsupported");
        assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_ai_action_applies_user_regex_to_redaction() {
        // Regression for the leak where `run_ai_action` redacted only the
        // built-in patterns (`Redactor::redact`) and silently passed any
        // `regex_denylist` match through to the AI provider. After the
        // fix, the redactor must be settings-aware so user-supplied rules
        // apply uniformly — even on entries classified before the regex
        // was added.
        let (runtime, _) = runtime_with_memory_clipboard();
        // Step 1: add the entry under default (empty) regex_denylist so it
        // lands as Public — Issue 1 ensures any UserRegex match instead
        // gets dropped at capture time.
        let id = runtime
            .add_text("ticket INTERNAL-42 stays".to_owned())
            .await
            .expect("public entry should be added");
        // Step 2: enable AI and configure the regex denylist *after* the
        // entry is in the DB, mirroring "user adds a rule then runs an AI
        // action on an old clip".
        runtime
            .save_settings(AppSettings {
                ai_enabled: true,
                ai_provider: AiProviderSetting::Local,
                regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
                ..AppSettings::default()
            })
            .await
            .expect("save settings");

        // Summarize has require_redaction=true, so even Public input must
        // be redacted before reaching the provider.
        let output = runtime
            .run_ai_action(id, AiActionId::Summarize)
            .await
            .expect("summarize should succeed");
        // MockAiProvider echoes the redacted input back as the output text,
        // which lets us assert exactly what the provider saw.
        assert!(
            !output.text.contains("INTERNAL-42"),
            "user regex must redact AI input, got: {}",
            output.text,
        );
        assert!(
            output.text.contains("[REDACTED]"),
            "expected redaction marker, got: {}",
            output.text,
        );
    }

    #[tokio::test]
    async fn run_ai_action_redact_secrets_applies_user_regex_on_public_entry() {
        // RedactSecrets used to keep `require_redaction = false`, which let
        // the local provider's bare `Redactor` (built-in patterns only) run
        // against the raw text of Public entries. Anything matched solely
        // by the user's `regex_denylist` slipped through unredacted. With
        // the policy bumped to `require_redaction = true`, run_ai_action
        // redacts via the settings-aware classifier before the provider
        // sees the input.
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("ticket INTERNAL-77 stays".to_owned())
            .await
            .expect("public entry should be added");
        runtime
            .save_settings(AppSettings {
                ai_enabled: true,
                ai_provider: AiProviderSetting::Local,
                regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
                ..AppSettings::default()
            })
            .await
            .expect("save settings");

        let output = runtime
            .run_ai_action(id, AiActionId::RedactSecrets)
            .await
            .expect("redact-secrets should succeed");

        assert!(
            !output.text.contains("INTERNAL-77"),
            "user regex must redact RedactSecrets input, got: {}",
            output.text,
        );
        assert!(
            output.text.contains("[REDACTED]"),
            "expected redaction marker, got: {}",
            output.text,
        );
    }

    #[tokio::test]
    async fn run_ai_action_blocked_when_input_exceeds_max_bytes() {
        let (runtime, _) = runtime_with_memory_clipboard();
        // Save with a roomy max_entry_size_bytes so add_text accepts the
        // long body, but ai_enabled=true + Local so we get past the policy
        // and hit the registry's 64 KiB max_bytes cap.
        runtime
            .store()
            .save_settings(AppSettings {
                ai_enabled: true,
                ai_provider: AiProviderSetting::Local,
                max_entry_size_bytes: 256 * 1024,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        let large = "a".repeat(65 * 1024);
        let id = runtime
            .add_text(large)
            .await
            .expect("large entry should be added");
        let err = runtime
            .run_ai_action(id, AiActionId::Summarize)
            .await
            .expect_err("must refuse inputs over max_bytes");
        assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
    }

    #[test]
    fn builder_build_errors_when_clipboard_missing() {
        // `build()` is the production entry point: a missing clipboard
        // adapter means the runtime would silently fall back to an
        // in-memory stub and the app would come up with capture
        // forever-disabled. Pin the contract that this returns
        // `AppError::Configuration` instead, so wiring drift is caught
        // at startup rather than as "clipboard quietly stopped working".
        let store = SqliteStore::open_memory().expect("memory store");
        let result = NagoriRuntime::builder(store)
            .paste(Arc::new(nagori_platform::NoopPasteController))
            .build();
        match result {
            Err(AppError::Configuration(ref msg)) if msg.contains("clipboard") => {}
            Err(err) => panic!("expected Configuration(clipboard), got {err:?}"),
            Ok(_) => panic!("expected error, builder accepted missing clipboard"),
        }
    }

    #[test]
    fn builder_build_errors_when_paste_missing() {
        // Symmetrically, a missing paste controller means
        // `paste_frontmost` would always be a no-op success on platforms
        // that forgot to wire their adapter. Surface this as
        // `AppError::Configuration` at build time.
        let store = SqliteStore::open_memory().expect("memory store");
        let result = NagoriRuntime::builder(store)
            .clipboard(Arc::new(MemoryClipboard::new()))
            .build();
        match result {
            Err(AppError::Configuration(ref msg)) if msg.contains("paste") => {}
            Err(err) => panic!("expected Configuration(paste), got {err:?}"),
            Ok(_) => panic!("expected error, builder accepted missing paste controller"),
        }
    }

    #[tokio::test]
    async fn paste_frontmost_returns_error_when_controller_reports_pasted_false() {
        // The default `NoopPasteController` returns `PasteResult{pasted: false,
        // message: ...}`. Historically `paste_frontmost` discarded the bool
        // and returned Ok(()), so non-macOS paths and any future "tried but
        // OS blocked" outcome silently looked like success. Regression: the
        // runtime must promote `pasted=false` to a Platform error so the UI
        // can warn the user instead of pretending to paste.
        let store = SqliteStore::open_memory().expect("memory store");
        let runtime = NagoriRuntime::builder(store).build_for_test();
        let err = runtime
            .paste_frontmost()
            .await
            .expect_err("Noop paste must surface as error");
        assert!(matches!(err, AppError::Platform(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn search_cache_serves_repeat_empty_query_without_round_tripping_storage() {
        // Empty query is the hottest path (palette open). The runtime must
        // serve the repeat call from the in-memory cache so SQLite isn't
        // touched once per keystroke.
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime
            .add_text("alpha".to_owned())
            .await
            .expect("seed entry");

        let first = runtime
            .search(SearchQuery::new("", String::new(), 10))
            .await
            .expect("first search");
        assert_eq!(first.len(), 1);
        assert_eq!(
            runtime.search_cache_handle().lock().unwrap().len(),
            1,
            "first search should populate the cache"
        );

        let second = runtime
            .search(SearchQuery::new("", String::new(), 10))
            .await
            .expect("repeat search");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].entry_id, first[0].entry_id);
    }

    #[tokio::test]
    async fn search_cache_invalidates_after_add_text() {
        // Invariant: any insert through the runtime must drop cached hits so
        // the next search reflects the new row. Without invalidation a freshly
        // captured clip wouldn't surface in the palette until the cache
        // happened to be flushed by some other mutation.
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime.add_text("alpha".to_owned()).await.expect("seed");
        let _ = runtime
            .search(SearchQuery::new("", String::new(), 10))
            .await
            .expect("warm cache");
        assert_eq!(runtime.search_cache_handle().lock().unwrap().len(), 1);

        runtime
            .add_text("beta".to_owned())
            .await
            .expect("second entry");
        assert!(
            runtime.search_cache_handle().lock().unwrap().is_empty(),
            "add_text must invalidate the search cache",
        );

        let results = runtime
            .search(SearchQuery::new("", String::new(), 10))
            .await
            .expect("post-insert search");
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_cache_invalidates_after_pin_toggle() {
        // `recent_entries` hoists pinned rows above plain ones, so toggling
        // the pin bit reorders the empty-query result. Stale cache hits would
        // hide the pin until something else cleared the cache.
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("alpha".to_owned())
            .await
            .expect("seed entry");
        let _ = runtime
            .search(SearchQuery::new("", String::new(), 10))
            .await
            .expect("warm cache");

        runtime
            .pin_entry(id, true)
            .await
            .expect("pin should succeed");
        assert!(
            runtime.search_cache_handle().lock().unwrap().is_empty(),
            "pin_entry must invalidate the search cache",
        );
    }

    #[derive(Debug)]
    struct StubPermissionChecker {
        check_response: std::sync::Mutex<Vec<PermissionStatus>>,
        check_observed_ctx: std::sync::Mutex<Option<PermissionCheckContext>>,
        request_response: std::sync::Mutex<PermissionStatus>,
        request_observed_prompt: std::sync::Mutex<Option<bool>>,
    }

    impl StubPermissionChecker {
        fn new(initial: Vec<PermissionStatus>, request: PermissionStatus) -> Self {
            Self {
                check_response: std::sync::Mutex::new(initial),
                check_observed_ctx: std::sync::Mutex::new(None),
                request_response: std::sync::Mutex::new(request),
                request_observed_prompt: std::sync::Mutex::new(None),
            }
        }

        fn set_check(&self, response: Vec<PermissionStatus>) {
            *self.check_response.lock().unwrap() = response;
        }

        fn set_request(&self, status: PermissionStatus) {
            *self.request_response.lock().unwrap() = status;
        }

        fn observed_ctx(&self) -> Option<PermissionCheckContext> {
            self.check_observed_ctx.lock().unwrap().clone()
        }

        fn observed_prompt(&self) -> Option<bool> {
            *self.request_observed_prompt.lock().unwrap()
        }
    }

    #[async_trait]
    impl PermissionChecker for StubPermissionChecker {
        async fn check(&self, ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>> {
            *self.check_observed_ctx.lock().unwrap() = Some(ctx.clone());
            Ok(self.check_response.lock().unwrap().clone())
        }

        async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus> {
            *self.request_observed_prompt.lock().unwrap() = Some(prompt);
            Ok(self.request_response.lock().unwrap().clone())
        }
    }

    fn accessibility_row(state: PermissionState) -> PermissionStatus {
        PermissionStatus {
            kind: PermissionKind::Accessibility,
            state,
            message: None,
            reason_code: None,
            setup_route: None,
            docs_url: None,
        }
    }

    #[tokio::test]
    async fn request_accessibility_stamps_prompted_at_when_prompt_true() {
        // The Phase A redesign keys the "NotRequested vs PromptShownNotGranted"
        // UI branch off `onboarding.accessibility_prompted_at`. Verify the
        // runtime persists that timestamp the first time we ask the host
        // to surface the TCC dialog (`prompt = true`).
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::NotDetermined)],
            accessibility_row(PermissionState::Denied),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();
        // Pre-condition: never prompted, so the context the checker sees
        // should be empty.
        let _ = runtime.permission_check().await.expect("permission_check");
        let ctx = stub.observed_ctx().expect("check was invoked");
        assert!(ctx.accessibility_prompted_at.is_none());

        let _ = runtime
            .request_accessibility(true)
            .await
            .expect("request_accessibility");
        assert_eq!(stub.observed_prompt(), Some(true));

        // Post-condition: the runtime persisted the prompt timestamp, and a
        // follow-up check carries it through the context so the checker
        // can discriminate Denied from NotDetermined.
        let settings = runtime.current_settings();
        assert!(
            settings.onboarding.accessibility_prompted_at.is_some(),
            "prompt = true must stamp accessibility_prompted_at",
        );
        let _ = runtime.permission_check().await.expect("permission_check");
        let ctx_after = stub.observed_ctx().expect("check was invoked");
        assert!(ctx_after.accessibility_prompted_at.is_some());
    }

    #[tokio::test]
    async fn request_accessibility_skips_prompted_at_when_prompt_false() {
        // `prompt = false` is the "just probe, don't surface UI" path
        // (`AXIsProcessTrustedWithOptions(prompt:NO)`); it must not move
        // the persisted prompt timestamp, otherwise a UI re-render that
        // calls the no-prompt probe would erroneously flip NotRequested.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::NotDetermined)],
            accessibility_row(PermissionState::Denied),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();

        let _ = runtime
            .request_accessibility(false)
            .await
            .expect("request_accessibility");
        assert_eq!(stub.observed_prompt(), Some(false));

        let settings = runtime.current_settings();
        assert!(
            settings.onboarding.accessibility_prompted_at.is_none(),
            "prompt = false must leave accessibility_prompted_at untouched",
        );
    }

    #[tokio::test]
    async fn permission_check_stamps_first_granted_once() {
        // `accessibility_first_granted_at` is a sticky onboarding marker:
        // once stamped, it must not be overwritten on subsequent grants
        // (the UI uses it for "you're set up" copy timing and onboarding
        // exit). Verify both the first-grant write and the no-op on a
        // second Granted observation.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::Granted)],
            accessibility_row(PermissionState::Granted),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();

        let _ = runtime.permission_check().await.expect("first check");
        let stamped = runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .expect("first Granted observation must stamp the marker");

        // Tick the clock through a short sleep so any rewrite would
        // produce a strictly-later timestamp.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let _ = runtime.permission_check().await.expect("second check");
        let after = runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .expect("marker remains set on subsequent grants");
        assert_eq!(stamped, after, "first_granted_at must be sticky");
    }

    #[tokio::test]
    async fn permission_check_does_not_stamp_when_not_granted() {
        // Symmetry with the sticky-marker test: a Denied / NotDetermined
        // observation must leave the marker absent, otherwise the Setup
        // card would skip its "Grant access" CTA and the doctor would
        // claim onboarding completed.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::Denied)],
            accessibility_row(PermissionState::Denied),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();

        let _ = runtime.permission_check().await.expect("check");
        assert!(
            runtime
                .current_settings()
                .onboarding
                .accessibility_first_granted_at
                .is_none(),
        );

        // Flip to Granted and re-check; the marker should now appear.
        stub.set_check(vec![accessibility_row(PermissionState::Granted)]);
        let _ = runtime.permission_check().await.expect("check after grant");
        assert!(
            runtime
                .current_settings()
                .onboarding
                .accessibility_first_granted_at
                .is_some(),
        );
    }

    #[tokio::test]
    async fn request_accessibility_stamps_first_granted_on_grant() {
        // The `request_accessibility` path also has to stamp the marker on
        // its own (rather than waiting for the next `permission_check`),
        // because the Setup card finishes its flow as soon as the trait
        // call resolves Granted — without this hook the marker would lag
        // by one full check cycle.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::NotDetermined)],
            accessibility_row(PermissionState::Granted),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();
        let _ = runtime
            .request_accessibility(true)
            .await
            .expect("request_accessibility");
        let onboarding = runtime.current_settings().onboarding;
        assert!(
            onboarding.accessibility_first_granted_at.is_some(),
            "Granted result must stamp first_granted_at without an extra permission_check"
        );
        assert!(onboarding.accessibility_prompted_at.is_some());
        // Flip the response back to Denied and re-call: the sticky marker
        // must not regress, even though the new observation is not Granted.
        stub.set_request(accessibility_row(PermissionState::Denied));
        let before = runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at;
        let _ = runtime
            .request_accessibility(true)
            .await
            .expect("request_accessibility");
        assert_eq!(
            runtime
                .current_settings()
                .onboarding
                .accessibility_first_granted_at,
            before,
            "first_granted_at must be sticky across later Denied results",
        );
    }

    #[tokio::test]
    async fn save_settings_preserves_persisted_onboarding_markers() {
        // The runtime owns the `onboarding` markers; an `update_settings`
        // IPC from the desktop shell (which round-trips a possibly-stale
        // snapshot of the markers) must never overwrite a marker that
        // the daemon stamped between the frontend's get_settings and
        // its follow-up update_settings. `save_settings` re-merges the
        // persisted `onboarding` block inside the write lock to enforce
        // that invariant.
        let store = SqliteStore::open_memory().expect("memory store should open");
        let stub = Arc::new(StubPermissionChecker::new(
            vec![accessibility_row(PermissionState::Granted)],
            accessibility_row(PermissionState::Granted),
        ));
        let runtime = NagoriRuntime::builder(store)
            .permissions(stub.clone())
            .build_for_test();
        // Stamp the marker via a permission_check observation.
        let _ = runtime.permission_check().await.expect("permission_check");
        let stamped = runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .expect("first_granted_at must be set after Granted observation");
        // Simulate a stale frontend snapshot: read settings, zero the
        // onboarding markers, then write back. The persisted markers
        // must survive.
        let mut stale = runtime.current_settings();
        stale.onboarding = OnboardingSettings::default();
        runtime
            .save_settings(stale)
            .await
            .expect("save_settings round-trip");
        let after = runtime.current_settings().onboarding;
        assert_eq!(
            after.accessibility_first_granted_at,
            Some(stamped),
            "save_settings must restore onboarding markers from the store",
        );
    }

    #[tokio::test]
    async fn search_cache_skips_long_queries() {
        // Long queries turn over too quickly to be worth caching, and would
        // crowd the small LRU. Verify we don't cache anything for a query
        // longer than `CACHEABLE_QUERY_LEN`.
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime
            .add_text("alphabetagamma".to_owned())
            .await
            .expect("seed");
        let long = "alphabetagamma".to_owned();
        let _ = runtime
            .search(SearchQuery::new(long.clone(), long, 10))
            .await
            .expect("search");
        assert!(
            runtime.search_cache_handle().lock().unwrap().is_empty(),
            "queries longer than the cache threshold must not populate the cache",
        );
    }
}
