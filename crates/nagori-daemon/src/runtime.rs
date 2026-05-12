use std::sync::Arc;

use nagori_ai::{AiActionRegistry, AiProvider, MockAiProvider};
use nagori_core::{
    AiActionId, AppError, AppSettings, AuditLog, ClipboardContent, ClipboardEntry, EntryFactory,
    EntryId, EntryRepository, PasteFormat, Result, SearchQuery, SearchResult, SecretAction,
    Sensitivity, SensitivityClassifier, SettingsRepository, is_text_safe_for_default_output,
    settings::AiProviderSetting,
};
use nagori_ipc::{
    AddEntryRequest, AiOutputDto, ClearRequest, ClearResponse, CopyEntryRequest,
    DeleteEntryRequest, DoctorPermission, DoctorReport, EntryDto, GetEntryRequest, HealthResponse,
    IpcError, IpcRequest, IpcResponse, ListPinnedRequest, ListRecentRequest, PasteEntryRequest,
    PinEntryRequest, RunAiActionRequest, SearchRequest, SearchResponse, SearchResultDto,
    UpdateSettingsRequest,
};
use nagori_platform::{
    ClipboardWriter, MemoryClipboard, NoopPasteController, PasteController, PermissionChecker,
    PermissionStatus,
};
use nagori_search::normalize_text;
use nagori_storage::SqliteStore;
use time::OffsetDateTime;
use tokio::sync::watch;
use tracing::error;

use crate::health::MaintenanceHealth;
use crate::search_cache::{
    CacheKey, CacheLookup, SharedSearchCache, lock_or_recover, new_shared_cache,
};

#[derive(Clone)]
pub struct NagoriRuntime {
    store: SqliteStore,
    clipboard: Arc<dyn ClipboardWriter>,
    paste: Arc<dyn PasteController>,
    ai: Arc<dyn AiProvider>,
    ai_registry: Arc<AiActionRegistry>,
    permissions: Option<Arc<dyn PermissionChecker>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    settings_tx: watch::Sender<AppSettings>,
    settings_rx: watch::Receiver<AppSettings>,
    socket_path: Arc<std::path::PathBuf>,
    /// Front-of-store LRU for recent search results. Hits skip the `SQLite`
    /// round-trip on the empty-query (`Recent`) and short-prefix paths;
    /// any corpus mutation invalidates it via [`Self::invalidate_search_cache`].
    search_cache: SharedSearchCache,
    /// Shared health snapshot of the background maintenance loop. The
    /// loop writes from `serve.rs` after each iteration; the IPC
    /// `Health` and `Doctor` handlers read it.
    maintenance_health: MaintenanceHealth,
}

impl NagoriRuntime {
    pub fn new(store: SqliteStore) -> Self {
        Self::builder(store).build()
    }

    pub fn builder(store: SqliteStore) -> NagoriRuntimeBuilder {
        NagoriRuntimeBuilder {
            store,
            clipboard: None,
            paste: None,
            ai: None,
            permissions: None,
            socket_path: None,
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
    pub async fn permission_check(&self) -> Result<Vec<PermissionStatus>> {
        match &self.permissions {
            Some(checker) => checker.check().await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn handle_ipc(&self, request: IpcRequest) -> IpcResponse {
        match self.handle_ipc_result(request).await {
            Ok(response) => response,
            Err(err) => IpcResponse::Error(IpcError {
                code: error_code(&err).to_owned(),
                message: err.to_string(),
                recoverable: !matches!(err, AppError::NotFound | AppError::Policy(_)),
            }),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_ipc_result(&self, request: IpcRequest) -> Result<IpcResponse> {
        match request {
            IpcRequest::Search(SearchRequest { query, limit }) => {
                let results = self
                    .search(SearchQuery::new(&query, normalize_text(&query), limit))
                    .await?
                    .into_iter()
                    .map(SearchResultDto::from)
                    .collect();
                Ok(IpcResponse::Search(SearchResponse { results }))
            }
            IpcRequest::GetEntry(GetEntryRequest {
                id,
                include_sensitive,
            }) => {
                let entry = self.get_entry(id).await?.ok_or(AppError::NotFound)?;
                let include_text =
                    include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
                Ok(IpcResponse::Entry(EntryDto::from_entry(
                    entry,
                    include_text,
                )))
            }
            IpcRequest::ListRecent(ListRecentRequest {
                limit,
                include_sensitive,
            }) => {
                let entries = self
                    .list_recent(limit)
                    .await?
                    .into_iter()
                    .map(|entry| {
                        let include_text =
                            include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
                        EntryDto::from_entry(entry, include_text)
                    })
                    .collect();
                Ok(IpcResponse::Entries(entries))
            }
            IpcRequest::ListPinned(ListPinnedRequest { include_sensitive }) => {
                let entries = self
                    .list_pinned()
                    .await?
                    .into_iter()
                    .map(|entry| {
                        let include_text =
                            include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
                        EntryDto::from_entry(entry, include_text)
                    })
                    .collect();
                Ok(IpcResponse::Entries(entries))
            }
            IpcRequest::AddEntry(AddEntryRequest { text }) => {
                let id = self.add_text(text).await?;
                let entry = self.get_entry(id).await?.ok_or(AppError::NotFound)?;
                let include_text = is_text_safe_for_default_output(entry.sensitivity);
                Ok(IpcResponse::Entry(EntryDto::from_entry(
                    entry,
                    include_text,
                )))
            }
            IpcRequest::CopyEntry(CopyEntryRequest { id }) => {
                self.copy_entry(id).await?;
                Ok(IpcResponse::Ack)
            }
            IpcRequest::PasteEntry(PasteEntryRequest { id, format }) => {
                self.paste_entry(id, format).await?;
                Ok(IpcResponse::Ack)
            }
            IpcRequest::DeleteEntry(DeleteEntryRequest { id }) => {
                self.delete_entry(id).await?;
                Ok(IpcResponse::Ack)
            }
            IpcRequest::PinEntry(PinEntryRequest { id, pinned }) => {
                self.pin_entry(id, pinned).await?;
                Ok(IpcResponse::Ack)
            }
            IpcRequest::RunAiAction(RunAiActionRequest { id, action }) => {
                let output = self.run_ai_action(id, action).await?;
                Ok(IpcResponse::AiOutput(AiOutputDto::from(output)))
            }
            IpcRequest::GetSettings => {
                let settings = self.get_settings().await?;
                Ok(IpcResponse::Settings(settings))
            }
            IpcRequest::UpdateSettings(UpdateSettingsRequest { value }) => {
                let settings: AppSettings = serde_json::from_value(value)
                    .map_err(|err| AppError::InvalidInput(err.to_string()))?;
                self.save_settings(settings).await?;
                Ok(IpcResponse::Ack)
            }
            IpcRequest::Clear(request) => {
                let cutoff = match request {
                    ClearRequest::All => OffsetDateTime::now_utc(),
                    ClearRequest::OlderThanDays { days } => {
                        OffsetDateTime::now_utc() - time::Duration::days(i64::from(days))
                    }
                };
                self.invalidate_search_cache();
                let deleted = self.store.clear_older_than(cutoff).await?;
                self.invalidate_search_cache();
                Ok(IpcResponse::Cleared(ClearResponse { deleted }))
            }
            IpcRequest::Doctor => Ok(IpcResponse::Doctor(self.build_doctor_report().await?)),
            IpcRequest::Health => {
                let maintenance = self.maintenance_health.report();
                // `ok` flips to false once retention is wedged so simple
                // health probes (load balancers, oncall checks) light up
                // without needing to inspect the nested struct.
                Ok(IpcResponse::Health(HealthResponse {
                    ok: !maintenance.degraded,
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    maintenance,
                }))
            }
            IpcRequest::Shutdown => {
                self.shutdown_handle().cancel();
                Ok(IpcResponse::Ack)
            }
        }
    }

    async fn build_doctor_report(&self) -> Result<DoctorReport> {
        let settings = self.current_settings();
        let mut permissions = Vec::new();
        if let Some(checker) = &self.permissions
            && let Ok(statuses) = checker.check().await
        {
            for status in statuses {
                permissions.push(DoctorPermission {
                    kind: format!("{:?}", status.kind),
                    state: format!("{:?}", status.state),
                    message: status.message,
                });
            }
        }
        let provider_label = match &settings.ai_provider {
            AiProviderSetting::None => "none".to_owned(),
            AiProviderSetting::Local => "local".to_owned(),
            AiProviderSetting::Remote { name } => format!("remote:{name}"),
        };
        // Probe the GitHub Releases API for the latest tag so `nagori
        // doctor` can show whether an update is available. Best-effort:
        // the probe is gated on macOS (the only target where the
        // updater plugin actually runs in MVP) and skipped when the
        // user has either disabled background update checks or opted
        // into `local_only_mode`. The call is capped by a short
        // timeout, and any error (offline, rate-limited, malformed
        // payload) collapses to `None` so doctor still completes.
        let latest_version =
            if cfg!(target_os = "macos") && settings.auto_update_check && !settings.local_only_mode
            {
                fetch_latest_release_version().await
            } else {
                None
            };
        Ok(DoctorReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            db_path: String::new(),
            socket_path: self.socket_path.display().to_string(),
            capture_enabled: settings.capture_enabled,
            auto_paste_enabled: settings.auto_paste_enabled,
            ai_enabled: settings.ai_enabled,
            local_only_mode: settings.local_only_mode,
            ai_provider: provider_label,
            permissions,
            maintenance: self.maintenance_health.report(),
            update_channel: settings.update_channel.as_str().to_owned(),
            latest_version,
        })
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
        // Image content survives capture as a `payload_blob` row plus an
        // `ImageContent` whose `pending_bytes` is dropped on deserialise, so
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
            PasteFormat::Preserve => self.clipboard.write_entry(&entry).await?,
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

    pub async fn get_settings(&self) -> Result<AppSettings> {
        self.store.get_settings().await
    }

    /// Persist updated settings *and* re-publish them on the watch channel
    /// so the capture loop and other subscribers pick up the change without
    /// the caller having to remember the second step.
    pub async fn save_settings(&self, settings: AppSettings) -> Result<()> {
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
        if !settings.ai_enabled {
            return Err(AppError::Policy(
                "ai actions are disabled in settings".to_owned(),
            ));
        }
        let action_def = self
            .ai_registry
            .get(action)
            .ok_or_else(|| AppError::InvalidInput(format!("unknown ai action {action:?}")))?;
        let policy = action_def.input_policy.clone();
        // Provider gating: `None` blocks everything; `Local` allows only
        // local-only actions; `Remote` is blocked when the user enabled
        // `local_only_mode` or when the action's `allow_remote=false`.
        let is_remote = match &settings.ai_provider {
            AiProviderSetting::None => {
                return Err(AppError::Policy(
                    "ai_provider is set to None — refusing to run".to_owned(),
                ));
            }
            AiProviderSetting::Local => false,
            AiProviderSetting::Remote { .. } => {
                if settings.local_only_mode {
                    return Err(AppError::Policy(
                        "local_only_mode is on — remote ai_provider blocked".to_owned(),
                    ));
                }
                if !policy.allow_remote {
                    return Err(AppError::Policy(format!(
                        "action {} disallows remote providers",
                        action_def.name
                    )));
                }
                true
            }
        };
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
                if policy.require_redaction || is_remote {
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

    pub fn build(self) -> NagoriRuntime {
        let (settings_tx, settings_rx) = watch::channel(AppSettings::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        NagoriRuntime {
            store: self.store,
            clipboard: self
                .clipboard
                .unwrap_or_else(|| Arc::new(MemoryClipboard::new())),
            paste: self.paste.unwrap_or_else(|| Arc::new(NoopPasteController)),
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

const fn error_code(err: &AppError) -> &'static str {
    match err {
        AppError::Storage(_) => "storage_error",
        AppError::Search(_) => "search_error",
        AppError::Platform(_) => "platform_error",
        AppError::Permission(_) => "permission_error",
        AppError::Ai(_) => "ai_error",
        AppError::Policy(_) => "policy_error",
        AppError::NotFound => "not_found",
        AppError::InvalidInput(_) => "invalid_input",
        AppError::Unsupported(_) => "unsupported",
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

/// Best-effort lookup of the latest released `nagori` tag on GitHub.
///
/// The doctor handler calls this to surface "you're behind" without
/// shelling out to the desktop updater. Strict timeout, no retries:
/// if GitHub is unreachable, rate-limiting us, or returns an
/// unexpected payload, we return `None` and doctor renders
/// "(unknown)" rather than failing the whole report.
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
    use nagori_ipc::{AddEntryRequest, EntryDto, GetEntryRequest};
    use nagori_platform::{MemoryClipboard, PasteResult};

    use super::*;

    fn runtime_with_memory_clipboard() -> (NagoriRuntime, Arc<MemoryClipboard>) {
        let store = SqliteStore::open_memory().expect("memory store should open");
        let clipboard = Arc::new(MemoryClipboard::new());
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .build();
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
            .build();
        (runtime, clipboard)
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
    async fn run_ai_action_blocked_when_ai_disabled() {
        // Default settings have ai_enabled=false. Even with ai_provider=Local,
        // run_ai_action must refuse rather than calling the provider — the
        // master switch wins.
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text("hello".to_owned())
            .await
            .expect("entry should be added");
        let err = runtime
            .run_ai_action(id, AiActionId::Summarize)
            .await
            .expect_err("ai actions must be refused when ai_enabled=false");
        assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_ai_action_blocked_when_remote_provider_in_local_only_mode() {
        let (runtime, _) = runtime_with_memory_clipboard();
        runtime
            .store()
            .save_settings(AppSettings {
                ai_enabled: true,
                local_only_mode: true,
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
            .run_ai_action(id, AiActionId::Summarize)
            .await
            .expect_err("local_only_mode must veto remote providers");
        assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
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

    #[tokio::test]
    async fn paste_frontmost_returns_error_when_controller_reports_pasted_false() {
        // The default `NoopPasteController` returns `PasteResult{pasted: false,
        // message: ...}`. Historically `paste_frontmost` discarded the bool
        // and returned Ok(()), so non-macOS paths and any future "tried but
        // OS blocked" outcome silently looked like success. Regression: the
        // runtime must promote `pasted=false` to a Platform error so the UI
        // can warn the user instead of pretending to paste.
        let store = SqliteStore::open_memory().expect("memory store");
        let runtime = NagoriRuntime::builder(store).build();
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
